//! The `{{ … }}` / `[[ … ]]` template engine.
//!
//! Two deliberately small facilities, and nothing more:
//!
//! - `{{ path.to.var }}` — a dotted lookup over a nested context. A value that is
//!   a *single* whole-string placeholder returns the value with its type intact
//!   (a list or an integer survives); an embedded placeholder is stringified.
//! - `[[ expr ]]` — a minimal numeric expression, kept for real needs like
//!   `[[ max(1, system.memory.total_gb // 4) ]]`. It is not a general expression
//!   language: no `**`, no bitwise ops, no boolean connectives, no arbitrary
//!   names (conditions are a structured match elsewhere, not here).
//!
//! The engine is zero-domain: it knows about a [`Value`] tree and two bits of
//! syntax, nothing about projects, toolchains, or files. That keeps it reusable
//! and trivially testable in isolation. Context values may themselves be
//! templates, so lookups resolve lazily and recursively, memoised, with cycle
//! detection — which is why there is no separate "dependency map" or topological
//! pass: one entry referencing another simply resolves on demand.
//!
//! Every failure is hard (an unknown path, a cycle, a type mismatch, a division
//! by zero). Callers are expected to hand in a *fully populated* context, so a
//! missing path always signals a real mistake rather than degrading to "".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum TemplateError {
    #[error("cannot resolve path '{0}' in template context")]
    UnknownPath(String),
    #[error("circular reference: {0}")]
    Cycle(String),
    #[error("cannot use a {kind} value ('{path}') inside a string")]
    NotAScalar { path: String, kind: &'static str },
    #[error("expression error: {0}")]
    Expr(String),
}

/// A template context value. This is the codebase's own tree rather than
/// `toml::Value` because a context is assembled from more than a config file —
/// computed system facts, resolved paths, and so on — and needs one uniform type.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Self {
        Value::Str(s.into())
    }

    /// A convenience for building a map context from `(key, value)` pairs.
    pub fn map<I, K>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        Value::Map(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    fn kind(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::Bool(_) => "boolean",
            Value::List(_) => "list",
            Value::Map(_) => "map",
        }
    }

    /// Render a scalar for embedding inside a larger string. Collections have no
    /// sensible inline form, so they are refused rather than guessed at.
    fn as_embedded(&self, path: &str) -> Result<String, TemplateError> {
        Ok(match self {
            Value::Str(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Bool(b) => b.to_string(), // lowercase true/false
            other => {
                return Err(TemplateError::NotAScalar {
                    path: path.to_owned(),
                    kind: other.kind(),
                })
            }
        })
    }

    /// Insert `value` at a dotted `path`, creating intermediate maps. Used to
    /// build a context programmatically.
    pub fn insert_path(&mut self, path: &str, value: Value) {
        let mut cur = self;
        let mut parts = path.split('.').peekable();
        while let Some(part) = parts.next() {
            let Value::Map(map) = cur else {
                return;
            };
            if parts.peek().is_none() {
                map.insert(part.to_owned(), value);
                return;
            }
            cur = map
                .entry(part.to_owned())
                .or_insert_with(|| Value::Map(BTreeMap::new()));
        }
    }
}

impl From<toml::Value> for Value {
    fn from(v: toml::Value) -> Self {
        match v {
            toml::Value::String(s) => Value::Str(s),
            toml::Value::Integer(n) => Value::Int(n),
            toml::Value::Float(f) => Value::Float(f),
            toml::Value::Boolean(b) => Value::Bool(b),
            toml::Value::Datetime(d) => Value::Str(d.to_string()),
            toml::Value::Array(a) => Value::List(a.into_iter().map(Value::from).collect()),
            toml::Value::Table(t) => {
                Value::Map(t.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

impl From<&toml::Value> for Value {
    fn from(v: &toml::Value) -> Self {
        match v {
            toml::Value::String(s) => Value::Str(s.clone()),
            toml::Value::Integer(n) => Value::Int(*n),
            toml::Value::Float(f) => Value::Float(*f),
            toml::Value::Boolean(b) => Value::Bool(*b),
            toml::Value::Datetime(d) => Value::Str(d.to_string()),
            toml::Value::Array(a) => Value::List(a.iter().map(Value::from).collect()),
            toml::Value::Table(t) => {
                Value::Map(t.iter().map(|(k, v)| (k.clone(), Value::from(v))).collect())
            }
        }
    }
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

/// Resolves templates against a nested context. Holds the raw context (whose
/// values may be templates) plus a memo of already-resolved paths.
pub struct Engine {
    root: Value,
    cache: RefCell<HashMap<String, Value>>,
}

impl Engine {
    pub fn new(context: Value) -> Self {
        Self {
            root: context,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve an arbitrary value: strings have their templates expanded, lists
    /// and maps are walked element-wise, scalars pass through.
    pub fn resolve(&self, raw: &Value) -> Result<Value, TemplateError> {
        self.resolve_value(raw, &mut Vec::new())
    }

    /// Resolve a single template string. A whole-string placeholder or
    /// expression yields a typed value; anything else yields a string.
    pub fn resolve_str(&self, s: &str) -> Result<Value, TemplateError> {
        self.resolve_string(s, &mut Vec::new())
    }

    /// Look up (and fully resolve) a dotted context path.
    pub fn get(&self, path: &str) -> Result<Value, TemplateError> {
        self.resolve_path(path, &mut Vec::new())
    }

    fn resolve_value(&self, v: &Value, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        match v {
            Value::Str(s) => self.resolve_string(s, stack),
            Value::List(items) => Ok(Value::List(
                items
                    .iter()
                    .map(|item| self.resolve_value(item, stack))
                    .collect::<Result<_, _>>()?,
            )),
            Value::Map(map) => Ok(Value::Map(
                map.iter()
                    .map(|(k, val)| Ok((k.clone(), self.resolve_value(val, stack)?)))
                    .collect::<Result<_, TemplateError>>()?,
            )),
            scalar => Ok(scalar.clone()),
        }
    }

    fn resolve_string(&self, s: &str, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        if let Some(expr) = whole_delimited(s, "[[", "]]") {
            // Inside `[[ … ]]` a bare dotted identifier is a context lookup; the
            // engine hands the evaluator a resolver so paths resolve through the
            // same lazy/cycle-checked machinery as everywhere else.
            let mut err: Option<TemplateError> = None;
            let mut lookup = |path: &str| -> Result<Value, String> {
                self.resolve_path(path, stack).map_err(|e| {
                    let msg = e.to_string();
                    err = Some(e);
                    msg
                })
            };
            return expr::eval(expr, &mut lookup).map_err(|msg| match err.take() {
                Some(inner) => inner,
                None => TemplateError::Expr(msg),
            });
        }
        if let Some(path) = whole_delimited(s, "{{", "}}") {
            return self.resolve_path(path.trim(), stack);
        }
        Ok(Value::Str(self.substitute(s, stack)?))
    }

    /// Expand every `{{ … }}` in `text`, stringifying each scalar in place.
    fn substitute(&self, text: &str, stack: &mut Vec<String>) -> Result<String, TemplateError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find("{{") {
            out.push_str(&rest[..open]);
            let after = &rest[open + 2..];
            let close = after
                .find("}}")
                .ok_or_else(|| TemplateError::Expr(format!("unterminated '{{{{' in '{text}'")))?;
            let path = after[..close].trim();
            let value = self.resolve_path(path, stack)?;
            out.push_str(&value.as_embedded(path)?);
            rest = &after[close + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn resolve_path(&self, path: &str, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        if let Some(hit) = self.cache.borrow().get(path) {
            return Ok(hit.clone());
        }
        if stack.iter().any(|p| p == path) {
            stack.push(path.to_owned());
            return Err(TemplateError::Cycle(stack.join(" -> ")));
        }
        let raw = lookup_raw(&self.root, path)?.clone();
        stack.push(path.to_owned());
        let resolved = self.resolve_value(&raw, stack);
        stack.pop();
        let resolved = resolved?;
        self.cache
            .borrow_mut()
            .insert(path.to_owned(), resolved.clone());
        Ok(resolved)
    }
}

/// If `s` is exactly `<open> … <close>` (ignoring surrounding whitespace) and
/// nothing else, return the inner text. This is what distinguishes a
/// whole-string placeholder (typed result) from an embedded one (stringified).
fn whole_delimited<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let t = s.trim();
    let inner = t.strip_prefix(open)?.strip_suffix(close)?;
    // Reject a second opener, so "{{a}}{{b}}" is treated as embedded, not whole.
    if inner.contains(open) {
        None
    } else {
        Some(inner)
    }
}

fn lookup_raw<'a>(root: &'a Value, path: &str) -> Result<&'a Value, TemplateError> {
    let mut cur = root;
    for part in path.split('.') {
        match cur {
            Value::Map(map) => {
                cur = map
                    .get(part)
                    .ok_or_else(|| TemplateError::UnknownPath(path.to_owned()))?;
            }
            Value::List(items) => {
                let idx: usize = part
                    .parse()
                    .map_err(|_| TemplateError::UnknownPath(path.to_owned()))?;
                cur = items
                    .get(idx)
                    .ok_or_else(|| TemplateError::UnknownPath(path.to_owned()))?;
            }
            _ => return Err(TemplateError::UnknownPath(path.to_owned())),
        }
    }
    Ok(cur)
}

/// The `[[ … ]]` expression sublanguage: lex, parse (recursive descent), and
/// evaluate. Kept in its own module so the surface stays visibly small. A bare
/// dotted identifier is a context path, resolved through the `lookup` callback;
/// an identifier immediately followed by `(` is a function call.
mod expr {
    use super::{format_float, Value};

    pub type Lookup<'a> = dyn FnMut(&str) -> Result<Value, String> + 'a;

    pub fn eval(src: &str, lookup: &mut Lookup<'_>) -> Result<Value, String> {
        let tokens = lex(src)?;
        let n = tokens.len();
        let mut parser = Parser {
            tokens,
            pos: 0,
            lookup,
        };
        let value = parser.expr()?;
        if parser.pos != n {
            return Err(format!("unexpected trailing tokens in '{src}'"));
        }
        Ok(value)
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        Int(i64),
        Float(f64),
        Str(String),
        Ident(String),
        Plus,
        Minus,
        Star,
        Slash,
        SlashSlash,
        Percent,
        Eq,
        Ne,
        Lt,
        Le,
        Gt,
        Ge,
        LParen,
        RParen,
        Comma,
    }

    fn lex(src: &str) -> Result<Vec<Tok>, String> {
        let mut toks = Vec::new();
        let chars: Vec<char> = src.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match c {
                c if c.is_whitespace() => i += 1,
                '+' => {
                    toks.push(Tok::Plus);
                    i += 1;
                }
                '-' => {
                    toks.push(Tok::Minus);
                    i += 1;
                }
                '*' => {
                    toks.push(Tok::Star);
                    i += 1;
                }
                '%' => {
                    toks.push(Tok::Percent);
                    i += 1;
                }
                '(' => {
                    toks.push(Tok::LParen);
                    i += 1;
                }
                ')' => {
                    toks.push(Tok::RParen);
                    i += 1;
                }
                ',' => {
                    toks.push(Tok::Comma);
                    i += 1;
                }
                '/' => {
                    if chars.get(i + 1) == Some(&'/') {
                        toks.push(Tok::SlashSlash);
                        i += 2;
                    } else {
                        toks.push(Tok::Slash);
                        i += 1;
                    }
                }
                '=' if chars.get(i + 1) == Some(&'=') => {
                    toks.push(Tok::Eq);
                    i += 2;
                }
                '!' if chars.get(i + 1) == Some(&'=') => {
                    toks.push(Tok::Ne);
                    i += 2;
                }
                '<' if chars.get(i + 1) == Some(&'=') => {
                    toks.push(Tok::Le);
                    i += 2;
                }
                '>' if chars.get(i + 1) == Some(&'=') => {
                    toks.push(Tok::Ge);
                    i += 2;
                }
                '<' => {
                    toks.push(Tok::Lt);
                    i += 1;
                }
                '>' => {
                    toks.push(Tok::Gt);
                    i += 1;
                }
                '"' => {
                    let mut s = String::new();
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\' && i + 1 < chars.len() {
                            i += 1;
                        }
                        s.push(chars[i]);
                        i += 1;
                    }
                    if i >= chars.len() {
                        return Err("unterminated string literal".into());
                    }
                    i += 1; // closing quote
                    toks.push(Tok::Str(s));
                }
                c if c.is_ascii_digit() => {
                    let start = i;
                    let mut is_float = false;
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                        if chars[i] == '.' {
                            is_float = true;
                        }
                        i += 1;
                    }
                    let text: String = chars[start..i].iter().collect();
                    if is_float {
                        toks.push(Tok::Float(
                            text.parse().map_err(|_| format!("bad number '{text}'"))?,
                        ));
                    } else {
                        toks.push(Tok::Int(
                            text.parse().map_err(|_| format!("bad number '{text}'"))?,
                        ));
                    }
                }
                c if c.is_alphabetic() || c == '_' => {
                    // Dotted paths (`system.memory.total_gb`) are a single ident.
                    let start = i;
                    while i < chars.len()
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                    {
                        i += 1;
                    }
                    toks.push(Tok::Ident(chars[start..i].iter().collect()));
                }
                other => return Err(format!("unexpected character '{other}'")),
            }
        }
        Ok(toks)
    }

    struct Parser<'a> {
        tokens: Vec<Tok>,
        pos: usize,
        lookup: &'a mut Lookup<'a>,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<&Tok> {
            self.tokens.get(self.pos)
        }
        fn next(&mut self) -> Option<Tok> {
            let t = self.tokens.get(self.pos).cloned();
            if t.is_some() {
                self.pos += 1;
            }
            t
        }
        fn eat(&mut self, tok: &Tok) -> Result<(), String> {
            if self.peek() == Some(tok) {
                self.pos += 1;
                Ok(())
            } else {
                Err(format!("expected {tok:?}"))
            }
        }

        // expr := additive ( cmp additive )?
        fn expr(&mut self) -> Result<Value, String> {
            let left = self.additive()?;
            let op = match self.peek() {
                Some(Tok::Eq) => Cmp::Eq,
                Some(Tok::Ne) => Cmp::Ne,
                Some(Tok::Lt) => Cmp::Lt,
                Some(Tok::Le) => Cmp::Le,
                Some(Tok::Gt) => Cmp::Gt,
                Some(Tok::Ge) => Cmp::Ge,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.additive()?;
            compare(op, &left, &right)
        }

        fn additive(&mut self) -> Result<Value, String> {
            let mut acc = self.multiplicative()?;
            loop {
                let op = match self.peek() {
                    Some(Tok::Plus) => '+',
                    Some(Tok::Minus) => '-',
                    _ => break,
                };
                self.pos += 1;
                let rhs = self.multiplicative()?;
                acc = arith(op, &acc, &rhs)?;
            }
            Ok(acc)
        }

        fn multiplicative(&mut self) -> Result<Value, String> {
            let mut acc = self.unary()?;
            loop {
                let op = match self.peek() {
                    Some(Tok::Star) => '*',
                    Some(Tok::Slash) => '/',
                    Some(Tok::SlashSlash) => 'F', // floor-div
                    Some(Tok::Percent) => '%',
                    _ => break,
                };
                self.pos += 1;
                let rhs = self.unary()?;
                acc = arith(op, &acc, &rhs)?;
            }
            Ok(acc)
        }

        fn unary(&mut self) -> Result<Value, String> {
            match self.peek() {
                Some(Tok::Minus) => {
                    self.pos += 1;
                    let v = self.unary()?;
                    arith('-', &Value::Int(0), &v)
                }
                Some(Tok::Plus) => {
                    self.pos += 1;
                    self.unary()
                }
                _ => self.primary(),
            }
        }

        fn primary(&mut self) -> Result<Value, String> {
            match self.next() {
                Some(Tok::Int(n)) => Ok(Value::Int(n)),
                Some(Tok::Float(f)) => Ok(Value::Float(f)),
                Some(Tok::Str(s)) => Ok(Value::Str(s)),
                Some(Tok::LParen) => {
                    let v = self.expr()?;
                    self.eat(&Tok::RParen)?;
                    Ok(v)
                }
                Some(Tok::Ident(name)) => match name.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    // `name(` is a function call; a bare identifier is a context path.
                    _ if self.peek() == Some(&Tok::LParen) => {
                        self.pos += 1;
                        let mut args = Vec::new();
                        if self.peek() != Some(&Tok::RParen) {
                            args.push(self.expr()?);
                            while self.peek() == Some(&Tok::Comma) {
                                self.pos += 1;
                                args.push(self.expr()?);
                            }
                        }
                        self.eat(&Tok::RParen)?;
                        call(&name, args)
                    }
                    _ => (self.lookup)(&name),
                },
                other => Err(format!("unexpected {other:?}")),
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Cmp {
        Eq,
        Ne,
        Lt,
        Le,
        Gt,
        Ge,
    }

    fn as_f64(v: &Value) -> Option<f64> {
        match v {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn arith(op: char, a: &Value, b: &Value) -> Result<Value, String> {
        // Integer path keeps integer results (so `//` and `%` behave as expected
        // and paths built from them don't sprout ".0").
        if let (Value::Int(x), Value::Int(y)) = (a, b) {
            return Ok(match op {
                '+' => Value::Int(x + y),
                '-' => Value::Int(x - y),
                '*' => Value::Int(x * y),
                '/' => {
                    if *y == 0 {
                        return Err("division by zero".into());
                    }
                    Value::Float(*x as f64 / *y as f64)
                }
                'F' => {
                    if *y == 0 {
                        return Err("division by zero".into());
                    }
                    Value::Int(x.div_euclid(*y))
                }
                '%' => {
                    if *y == 0 {
                        return Err("division by zero".into());
                    }
                    Value::Int(x.rem_euclid(*y))
                }
                _ => unreachable!(),
            });
        }
        let (x, y) = (
            as_f64(a).ok_or_else(|| format!("non-numeric operand for '{op}'"))?,
            as_f64(b).ok_or_else(|| format!("non-numeric operand for '{op}'"))?,
        );
        Ok(match op {
            '+' => Value::Float(x + y),
            '-' => Value::Float(x - y),
            '*' => Value::Float(x * y),
            '/' | 'F' => {
                if y == 0.0 {
                    return Err("division by zero".into());
                }
                let d = x / y;
                Value::Float(if op == 'F' { d.floor() } else { d })
            }
            '%' => Value::Float(x % y),
            _ => unreachable!(),
        })
    }

    fn compare(op: Cmp, a: &Value, b: &Value) -> Result<Value, String> {
        let ord = match (as_f64(a), as_f64(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => match (a, b) {
                (Value::Str(x), Value::Str(y)) => x.partial_cmp(y),
                (Value::Bool(x), Value::Bool(y)) => x.partial_cmp(y),
                _ => return Err("cannot compare values of different types".into()),
            },
        };
        let ord = ord.ok_or_else(|| "incomparable values".to_string())?;
        use std::cmp::Ordering::*;
        Ok(Value::Bool(match op {
            Cmp::Eq => ord == Equal,
            Cmp::Ne => ord != Equal,
            Cmp::Lt => ord == Less,
            Cmp::Le => ord != Greater,
            Cmp::Gt => ord == Greater,
            Cmp::Ge => ord != Less,
        }))
    }

    fn call(name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "min" | "max" => {
                if args.is_empty() {
                    return Err(format!("{name}() needs at least one argument"));
                }
                let want_max = name == "max";
                let mut best = args[0].clone();
                for a in &args[1..] {
                    let cmp = compare(if want_max { Cmp::Gt } else { Cmp::Lt }, a, &best)?;
                    if cmp == Value::Bool(true) {
                        best = a.clone();
                    }
                }
                Ok(best)
            }
            "int" | "float" | "str" | "bool" => {
                if args.len() != 1 {
                    return Err(format!("{name}() takes exactly one argument"));
                }
                convert(name, &args[0])
            }
            other => Err(format!("unknown function '{other}'")),
        }
    }

    fn convert(name: &str, v: &Value) -> Result<Value, String> {
        Ok(match name {
            "int" => Value::Int(match v {
                Value::Int(n) => *n,
                Value::Float(f) => *f as i64,
                Value::Bool(b) => *b as i64,
                Value::Str(s) => s.trim().parse().map_err(|_| format!("int('{s}')"))?,
                _ => return Err("int() of a collection".into()),
            }),
            "float" => Value::Float(match v {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                Value::Bool(b) => *b as i64 as f64,
                Value::Str(s) => s.trim().parse().map_err(|_| format!("float('{s}')"))?,
                _ => return Err("float() of a collection".into()),
            }),
            "str" => Value::Str(match v {
                Value::Str(s) => s.clone(),
                Value::Int(n) => n.to_string(),
                Value::Float(f) => format_float(*f),
                Value::Bool(b) => b.to_string(),
                _ => return Err("str() of a collection".into()),
            }),
            "bool" => Value::Bool(match v {
                Value::Bool(b) => *b,
                Value::Int(n) => *n != 0,
                Value::Float(f) => *f != 0.0,
                Value::Str(s) => !s.is_empty(),
                _ => return Err("bool() of a collection".into()),
            }),
            _ => unreachable!(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Value {
        let mut root = Value::Map(BTreeMap::new());
        root.insert_path("project.name", Value::str("mesa"));
        root.insert_path("build_type", Value::str("debug"));
        root.insert_path("system.memory.total_gb", Value::Int(16));
        root.insert_path("branch.slug", Value::str("feature_x"));
        root.insert_path("repo.path", Value::str("/src/mesa"));
        // self-referential environment map
        root.insert_path("env.TOOLS", Value::str("/opt/tools"));
        root.insert_path("env.BIN", Value::str("{{env.TOOLS}}/bin"));
        root.insert_path("env.PATH", Value::str("{{env.BIN}}:/usr/bin"));
        root
    }

    #[test]
    fn whole_placeholder_keeps_type() {
        let e = Engine::new(ctx());
        assert_eq!(
            e.resolve_str("{{ system.memory.total_gb }}").unwrap(),
            Value::Int(16)
        );
    }

    #[test]
    fn embedded_placeholder_stringifies() {
        let e = Engine::new(ctx());
        assert_eq!(
            e.resolve_str("{{repo.path}}/_build/{{build_type}}")
                .unwrap(),
            Value::str("/src/mesa/_build/debug")
        );
    }

    #[test]
    fn lazy_self_reference_resolves() {
        let e = Engine::new(ctx());
        assert_eq!(
            e.get("env.PATH").unwrap(),
            Value::str("/opt/tools/bin:/usr/bin")
        );
    }

    #[test]
    fn cycle_is_detected() {
        let mut root = Value::Map(BTreeMap::new());
        root.insert_path("a", Value::str("{{b}}"));
        root.insert_path("b", Value::str("{{a}}"));
        let e = Engine::new(root);
        assert!(matches!(e.get("a"), Err(TemplateError::Cycle(_))));
    }

    #[test]
    fn unknown_path_is_hard_error() {
        let e = Engine::new(ctx());
        assert!(matches!(
            e.resolve_str("{{nope.missing}}"),
            Err(TemplateError::UnknownPath(_))
        ));
    }

    #[test]
    fn expression_arithmetic() {
        let e = Engine::new(ctx());
        assert_eq!(
            e.resolve_str("[[ max(1, system.memory.total_gb // 4) ]]")
                .unwrap(),
            Value::Int(4)
        );
        assert_eq!(e.resolve_str("[[ 1 + 2 * 3 ]]").unwrap(), Value::Int(7));
        assert_eq!(e.resolve_str("[[ (1 + 2) * 3 ]]").unwrap(), Value::Int(9));
        assert_eq!(e.resolve_str("[[ 7 % 3 ]]").unwrap(), Value::Int(1));
    }

    #[test]
    fn expression_comparison_and_funcs() {
        let e = Engine::new(ctx());
        assert_eq!(
            e.resolve_str("[[ build_type == \"debug\" ]]").unwrap(),
            Value::Bool(true)
        );
        assert_eq!(e.resolve_str("[[ max(1, 2) ]]").unwrap(), Value::Int(2));
        assert_eq!(e.resolve_str("[[ int(3.9) ]]").unwrap(), Value::Int(3));
    }

    #[test]
    fn division_by_zero_is_error() {
        let e = Engine::new(ctx());
        assert!(matches!(
            e.resolve_str("[[ 1 // 0 ]]"),
            Err(TemplateError::Expr(_))
        ));
    }
}
