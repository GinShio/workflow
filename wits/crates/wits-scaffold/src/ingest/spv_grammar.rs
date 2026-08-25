//! Select one extension out of `spirv.core.grammar.json`.
//!
//! The grammar is machine-generated, so this is exact rather than heuristic.
//! When it contains no matching entry, extraction falls back to the prose
//! specification.
//!
//! Membership is not spelled uniformly, which is the one thing to understand
//! here. The grammar names the extension on the **capabilities** it introduces:
//!
//! ```json
//! { "enumerant": "WidgetVND", "value": 6141,
//!   "extensions": ["SPV_VND_widget", "SPV_OTHER_widget"] }
//! ```
//!
//! but associates matching instructions and enumerants with those capabilities
//! instead, never naming the extension again:
//!
//! ```json
//! { "opname": "OpWidgetLoadVND", "opcode": 6145,
//!   "capabilities": ["WidgetVND"] }
//! ```
//!
//! So the capabilities are collected first and then used to pull in everything
//! they enable. Only capabilities *this* extension introduces count, so a
//! pre-existing one such as `Shader` can never drag in unrelated instructions.

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::{dedupe, number};
use crate::kinds::KindTable;
use crate::model::{Enumerant, KindGroup, SpvEncoding, SpvOpcode, SpvOperand, SpvPlane};

/// Extract `name`'s tokens from a grammar document.
pub fn extract(text: &str, name: &str, table: &KindTable) -> Result<(SpvPlane, Vec<String>)> {
    let grammar: Value = serde_json::from_str(text).context("grammar is not valid JSON")?;
    let mut notes = Vec::new();
    let mut plane = SpvPlane::new(name);

    let introduced: Vec<Enumerant> = enumerants_of(&grammar, "Capability")
        .filter(|entry| lists(entry, "extensions", name))
        .map(enumerant)
        .collect::<Result<_>>()?;
    let introduced_names: Vec<String> = introduced.iter().map(|c| c.name.clone()).collect();

    let belongs = |entry: &Value| -> bool {
        lists(entry, "extensions", name)
            || introduced_names
                .iter()
                .any(|capability| lists(entry, "capabilities", capability))
    };

    for kind in &table.kinds {
        let mut found: Vec<Enumerant> = if kind.name == "Capability" {
            introduced.clone()
        } else {
            enumerants_of(&grammar, &kind.name)
                .filter(|entry| belongs(entry))
                .map(enumerant)
                .collect::<Result<_>>()?
        };
        found = dedupe(
            found,
            |e| e.name.clone(),
            |e| e.value,
            &kind.name,
            &mut notes,
        );
        if found.is_empty() {
            continue;
        }
        plane.kinds.push(KindGroup {
            name: kind.name.clone(),
            meta: kind.meta.clone(),
            enumerants: found,
        });
    }

    let opcodes: Vec<SpvOpcode> = grammar
        .get("instructions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|entry| belongs(entry))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|entry| opcode(&grammar, entry))
        .collect::<Result<_>>()?;
    for opcode in dedupe_opcodes(opcodes, &mut notes) {
        if opcode.class == "Type-Declaration" {
            plane.types.push(opcode);
        } else {
            plane.operations.push(opcode);
        }
    }

    if plane.kinds.is_empty() && plane.types.is_empty() && plane.operations.is_empty() {
        bail!(
            "{name} contributes nothing to this grammar — check the spelling or read the \
             asciidoc instead"
        );
    }
    notes.push(
        "extracted from spirv.core.grammar.json, so the tokens are already in the public headers"
            .to_owned(),
    );
    Ok((plane, notes))
}

/// Every enumerant of one operand kind.
fn enumerants_of<'a>(grammar: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    grammar
        .get("operand_kinds")
        .and_then(Value::as_array)
        .map(|kinds| kinds.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(move |entry| entry.get("kind").and_then(Value::as_str) == Some(kind))
        .filter_map(|entry| entry.get("enumerants").and_then(Value::as_array))
        .flatten()
}

/// Whether `entry[field]` is an array containing `wanted`.
fn lists(entry: &Value, field: &str, wanted: &str) -> bool {
    entry
        .get(field)
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(wanted)))
}

fn strings(entry: &Value, field: &str) -> Vec<String> {
    entry
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn enumerant(entry: &Value) -> Result<Enumerant> {
    let name = entry
        .get("enumerant")
        .and_then(Value::as_str)
        .context("an enumerant has no name")?;
    Ok(Enumerant {
        name: name.to_owned(),
        aliases: strings(entry, "aliases"),
        value: number(entry.get("value").context("enumerant has no value")?)
            .with_context(|| format!("enumerant {name}"))?,
        requires: strings(entry, "capabilities"),
    })
}

fn opcode(grammar: &Value, entry: &Value) -> Result<SpvOpcode> {
    let name = entry
        .get("opname")
        .and_then(Value::as_str)
        .context("an instruction has no opname")?;
    let operands: Vec<SpvOperand> = entry
        .get("operands")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|operand| {
                    Ok(SpvOperand {
                        kind: operand
                            .get("kind")
                            .and_then(Value::as_str)
                            .context("instruction operand has no kind")?
                            .to_owned(),
                        name: operand
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        quantifier: operand
                            .get("quantifier")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let encoding = encoding_of(grammar, &operands);
    Ok(SpvOpcode {
        name: name.to_owned(),
        aliases: strings(entry, "aliases"),
        value: number(entry.get("opcode").context("instruction has no opcode")?)
            .with_context(|| format!("instruction {name}"))?,
        class: entry
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_owned(),
        operands,
        capabilities: strings(entry, "capabilities"),
        encoding,
        meta: Default::default(),
    })
}

#[derive(Default)]
struct OperandShape {
    min_words: usize,
    variable: bool,
    literal_words: Vec<usize>,
    compatible: bool,
    reason: Option<String>,
}

fn encoding_of(grammar: &Value, operands: &[SpvOperand]) -> SpvEncoding {
    let has_result_type = operands
        .iter()
        .any(|operand| operand.kind == "IdResultType");
    let has_result_id = operands.iter().any(|operand| operand.kind == "IdResult");
    let mut min_word_count = 1usize;
    let mut op_word_offset = 0usize;
    let mut variable_word_count = false;
    let mut literal_operands = Vec::new();
    let mut reasons = Vec::new();
    let mut positions_are_stable = true;

    for operand in operands {
        let shape = operand_shape(grammar, &operand.kind, &mut Vec::new());
        let optional = operand.quantifier.is_some();
        if !optional {
            min_word_count += shape.min_words;
        }
        variable_word_count |= optional || shape.variable;

        if operand.kind == "IdResultType" || operand.kind == "IdResult" {
            continue;
        }
        if !shape.compatible {
            reasons.push(
                shape
                    .reason
                    .unwrap_or_else(|| format!("unsupported operand kind {}", operand.kind)),
            );
        }
        if positions_are_stable {
            literal_operands.extend(
                shape
                    .literal_words
                    .iter()
                    .map(|offset| op_word_offset + offset),
            );
        } else if !shape.literal_words.is_empty() {
            reasons.push("literal operand follows a variable-width operand".to_owned());
        }
        if !optional {
            op_word_offset += shape.min_words;
        }
        positions_are_stable &= !optional && !shape.variable;
    }

    reasons.sort();
    reasons.dedup();

    SpvEncoding {
        has_result_type,
        has_result_id,
        min_word_count,
        variable_word_count,
        literal_operands,
        literal_indices_known: reasons.is_empty(),
        incompatibility: (!reasons.is_empty()).then(|| reasons.join("; ")),
    }
}

fn operand_shape(grammar: &Value, kind: &str, stack: &mut Vec<String>) -> OperandShape {
    if stack.iter().any(|entry| entry == kind) {
        return OperandShape {
            min_words: 1,
            compatible: false,
            reason: Some(format!("recursive operand kind {kind}")),
            ..Default::default()
        };
    }
    let Some(definition) = grammar
        .get("operand_kinds")
        .and_then(Value::as_array)
        .and_then(|kinds| {
            kinds
                .iter()
                .find(|entry| entry.get("kind").and_then(Value::as_str) == Some(kind))
        })
    else {
        return OperandShape {
            min_words: 1,
            compatible: false,
            reason: Some(format!("unknown operand kind {kind}")),
            ..Default::default()
        };
    };
    let category = definition
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match category {
        "Id" => OperandShape {
            min_words: 1,
            compatible: true,
            ..Default::default()
        },
        "Literal" => {
            let variable = matches!(
                kind,
                "LiteralInteger" | "LiteralString" | "LiteralContextDependentNumber"
            );
            let compatible = !matches!(kind, "LiteralString" | "LiteralContextDependentNumber");
            OperandShape {
                min_words: 1,
                variable,
                literal_words: vec![0],
                compatible,
                reason: (!compatible).then(|| format!("variable literal kind {kind}")),
            }
        }
        "ValueEnum" | "BitEnum" => {
            let has_parameters = definition
                .get("enumerants")
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|entry| entry.get("parameters").is_some()));
            OperandShape {
                min_words: 1,
                variable: has_parameters,
                literal_words: vec![0],
                compatible: !has_parameters,
                reason: has_parameters.then(|| format!("parameterized operand kind {kind}")),
            }
        }
        "Composite" => {
            stack.push(kind.to_owned());
            let mut combined = OperandShape {
                compatible: true,
                ..Default::default()
            };
            if let Some(bases) = definition.get("bases").and_then(Value::as_array) {
                for base in bases.iter().filter_map(Value::as_str) {
                    let shape = operand_shape(grammar, base, stack);
                    combined.literal_words.extend(
                        shape
                            .literal_words
                            .iter()
                            .map(|offset| combined.min_words + offset),
                    );
                    combined.min_words += shape.min_words;
                    combined.variable |= shape.variable;
                    if !shape.compatible {
                        combined.compatible = false;
                        combined.reason = shape.reason;
                    }
                }
            }
            stack.pop();
            combined
        }
        _ => OperandShape {
            min_words: 1,
            compatible: false,
            reason: Some(format!(
                "unsupported operand category {category} for {kind}"
            )),
            ..Default::default()
        },
    }
}

fn dedupe_opcodes(opcodes: Vec<SpvOpcode>, notes: &mut Vec<String>) -> Vec<SpvOpcode> {
    let mut kept: Vec<SpvOpcode> = Vec::new();
    for mut opcode in opcodes {
        if let Some(existing) = kept.iter_mut().find(|entry| entry.value == opcode.value) {
            let related = existing.name == opcode.name
                || existing.aliases.contains(&opcode.name)
                || opcode.aliases.contains(&existing.name);
            if related {
                existing.aliases.append(&mut opcode.aliases);
                if opcode.name != existing.name {
                    existing.aliases.push(opcode.name);
                }
                existing.aliases.sort();
                existing.aliases.dedup();
                existing.aliases.retain(|name| name != &existing.name);
            } else {
                notes.push(format!(
                    "opcode {}: {} and {} share the value; kept the first",
                    opcode.value, existing.name, opcode.name
                ));
            }
            continue;
        }
        if let Some(existing) = kept.iter().find(|entry| entry.name == opcode.name) {
            notes.push(format!(
                "instruction {}: conflicting values {} and {}, kept the first",
                opcode.name, existing.value, opcode.value
            ));
            continue;
        }
        opcode.aliases.sort();
        opcode.aliases.dedup();
        opcode.aliases.retain(|alias| alias != &opcode.name);
        kept.push(opcode);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[[kind]]
name = "Capability"
[kind.meta]
label = "A"

[[kind]]
name = "BuiltIn"
[kind.meta]
label = "B"
qualified = true
"#;

    /// The extension is named only on its capability; everything else hangs off
    /// that capability, matching the public grammar's membership model.
    const GRAMMAR: &str = r#"{
      "instructions": [
        { "opname": "OpFooEXT", "aliases": ["OpFooAliasEXT"], "class": "Arithmetic",
          "opcode": 6145, "capabilities": ["FooEXT"],
          "operands": [
            { "kind": "IdResultType" },
            { "kind": "IdResult" },
            { "kind": "IdRef", "name": "Input" }
          ] },
        { "opname": "OpTypeFooEXT", "aliases": ["OpTypeFooAliasEXT"],
          "class": "Type-Declaration", "opcode": 6146,
          "capabilities": ["FooEXT"],
          "operands": [{ "kind": "IdResult" }] },
        { "opname": "OpUnrelated", "opcode": 10, "capabilities": ["Shader"] }
      ],
      "operand_kinds": [
        { "kind": "IdResultType", "category": "Id" },
        { "kind": "IdResult", "category": "Id" },
        { "kind": "IdRef", "category": "Id" },
        { "kind": "Capability", "enumerants": [
            { "enumerant": "FooEXT", "aliases": ["FooAliasEXT"], "value": 6141,
              "extensions": ["SPV_TEST_foo", "SPV_OTHER_foo"] },
            { "enumerant": "Shader", "value": 1 }
        ]},
        { "kind": "BuiltIn", "enumerants": [
            { "enumerant": "FooIdEXT", "value": 6200, "capabilities": ["FooEXT"] },
            { "enumerant": "Position", "value": 0, "capabilities": ["Shader"] }
        ]},
        { "kind": "Decoration", "enumerants": [
            { "enumerant": "Ignored", "value": 5, "capabilities": ["FooEXT"] }
        ]}
      ]
    }"#;

    fn table() -> KindTable {
        KindTable::parse(TABLE).unwrap()
    }

    #[test]
    fn picks_up_the_capability_the_extension_names() {
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        let caps = plane.kind("Capability");
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].name, "FooEXT");
        assert_eq!(caps[0].aliases, ["FooAliasEXT"]);
        assert_eq!(caps[0].value, 6141);
    }

    #[test]
    fn reaches_operations_through_the_capability() {
        // The operation never names the extension, only `FooEXT`.
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        assert_eq!(plane.operations.len(), 1);
        assert_eq!(plane.operations[0].name, "OpFooEXT");
        assert_eq!(plane.operations[0].aliases, ["OpFooAliasEXT"]);
        let encoding = &plane.operations[0].encoding;
        assert!(encoding.literal_indices_known);
        assert_eq!(encoding.min_word_count, 4);
    }

    #[test]
    fn type_declarations_are_kept_separate_with_their_aliases() {
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        assert_eq!(plane.types.len(), 1);
        assert_eq!(plane.types[0].name, "OpTypeFooEXT");
        assert_eq!(plane.types[0].aliases, ["OpTypeFooAliasEXT"]);
        assert!(plane.types[0].encoding.literal_indices_known);
    }

    #[test]
    fn a_pre_existing_capability_drags_nothing_in() {
        // `Shader` is in the grammar but not introduced here, so neither
        // OpUnrelated nor BuiltIn Position may appear.
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        assert!(plane.operations.iter().all(|i| i.name != "OpUnrelated"));
        let builtins = plane
            .kinds
            .iter()
            .find(|k| k.name == "BuiltIn")
            .expect("BuiltIn group");
        assert_eq!(builtins.enumerants.len(), 1);
        assert_eq!(builtins.enumerants[0].name, "FooIdEXT");
    }

    #[test]
    fn only_kinds_in_the_table_are_collected() {
        // `Decoration` is enabled by FooEXT but absent from this tree's table,
        // so it must not appear — the table decides what gets wired up.
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        assert!(plane.kinds.iter().all(|k| k.name != "Decoration"));
    }

    #[test]
    fn per_kind_metadata_is_copied_from_the_table() {
        let (plane, _) = extract(GRAMMAR, "SPV_TEST_foo", &table()).unwrap();
        let builtins = plane.kinds.iter().find(|k| k.name == "BuiltIn").unwrap();
        assert_eq!(builtins.meta["label"].as_str(), Some("B"));
        assert_eq!(builtins.meta["qualified"].as_bool(), Some(true));
    }

    #[test]
    fn an_unknown_extension_is_an_error_pointing_at_the_asciidoc() {
        let err = extract(GRAMMAR, "SPV_TEST_absent", &table())
            .unwrap_err()
            .to_string();
        assert!(err.contains("asciidoc"), "got: {err}");
    }

    #[test]
    fn malformed_json_is_reported_as_such() {
        assert!(extract("{ not json", "SPV_TEST_foo", &table()).is_err());
    }

    #[test]
    fn an_operand_without_a_kind_is_rejected() {
        let grammar = r#"{
          "instructions": [{
            "opname": "OpBrokenTEST",
            "class": "Arithmetic",
            "opcode": 7000,
            "extensions": ["SPV_TEST_foo"],
            "operands": [{}]
          }]
        }"#;
        assert!(extract(grammar, "SPV_TEST_foo", &table()).is_err());
    }

    #[test]
    fn hex_values_parse() {
        let grammar = r#"{ "operand_kinds": [
            { "kind": "Capability", "enumerants": [
                { "enumerant": "FooEXT", "value": "0x0010", "extensions": ["SPV_TEST_foo"] }
            ]}
        ]}"#;
        let (plane, _) = extract(grammar, "SPV_TEST_foo", &table()).unwrap();
        assert_eq!(plane.kind("Capability")[0].value, 16);
    }
}
