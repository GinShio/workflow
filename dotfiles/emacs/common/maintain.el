;;; maintain.el --- install the packages the configuration declares -*- lexical-binding: t; -*-

;; Usage:
;;   emacs -Q --batch --init-directory=DIR -l maintain.el sync
;;   emacs -Q --batch --init-directory=DIR -l maintain.el upgrade
;;   emacs -Q --batch --init-directory=DIR -l maintain.el rebuild

;;; Commentary:

;; A build tool, not part of the configuration.  It never loads the
;; configuration: the `:straight' declarations are read out of the tangled
;; modules as data.  Nothing in a module is evaluated, so `:init', `:config'
;; and every other configuration form stay inert, and a bug in any of them
;; cannot block a sync.
;;
;; A recipe is never evaluated on either side -- `use-package' hands the
;; normalizer the source form, and both sides pass it to straight as data -- so
;; reading and expanding cannot disagree about what a declaration says.  They
;; can only disagree about which declarations exist, which is what
;; `ginshio-maintain--collect' refuses to guess at: a `use-package' form below
;; top level, or one in a file outside `ginshio-modules', is an error naming the
;; file and the package rather than a snapshot quietly short one entry.
;;
;; Startup, which does evaluate the modules, records the same declarations
;; through the `:straight' handler and compares them against the snapshot
;; written here; a drift between the two is a startup error.

;;; Code:

(require 'cl-lib)
(require 'subr-x)

(defconst ginshio-maintain-commands '("sync" "upgrade" "rebuild")
  "Commands `bin/ginshio-emacs' may ask this tool to perform.")

(defconst ginshio-maintain--root
  (file-name-as-directory
   (file-name-directory (or load-file-name buffer-file-name default-directory)))
  "The configuration this tool maintains.")

;; `ginshio-path' derives every directory from `user-emacs-directory', and it
;; is the one place the XDG contract is stated.  Point it at the configuration
;; before loading, so a run without --init-directory still works.
(setq user-emacs-directory ginshio-maintain--root)

(dolist (file '("ginshio-path.el" "ginshio-manifest.el"))
  (let ((path (expand-file-name (concat "modules/ginshio/" file)
                                ginshio-maintain--root)))
    (unless (file-readable-p path)
      (error "Missing %s; tangle the configuration first" path))
    (load path nil 'nomessage)))


;;;; Reading the declarations

(defun ginshio-maintain--read-forms (file)
  "Return every Lisp form in FILE, unevaluated."
  (with-temp-buffer
    (insert-file-contents file)
    (goto-char (point-min))
    (let (forms)
      (condition-case nil
          (while t (push (read (current-buffer)) forms))
        (end-of-file (nreverse forms))))))

(defun ginshio-maintain--straight-args (plist name file)
  "Return the forms `:straight' introduces in PLIST, or nil when absent.
NAME and FILE identify the declaration in errors.

`use-package' gathers every form between one keyword and the next, so
`:straight a b' hands the normalizer two recipes; `plist-get' would see
only the first and this tool would install one package fewer than startup
expects.  A repeated keyword is a different matter: merging two
occurrences needs a merge function this configuration never defines, so
`use-package' dies on it at startup with a message naming neither the
keyword nor the package.  Refusing it here says which."
  (let ((tail plist)
        (seen nil)
        args)
    (while tail
      (let ((keyword (car tail))
            (values nil))
        (setq tail (cdr tail))
        (while (and tail (not (keywordp (car tail))))
          (push (car tail) values)
          (setq tail (cdr tail)))
        (when (eq keyword :straight)
          (when seen
            (error "%s: `:straight' is given twice for `%s'"
                   (file-name-nondirectory file) name))
          (setq seen t
                args (nreverse values)))))
    args))

(defun ginshio-maintain--assert-top-level (form file depth)
  "Signal when FILE nests a `use-package' form below top level.
A nested declaration is invisible to this tool but visible to startup,
which would then disagree with the snapshot for reasons nobody can see."
  (when (consp form)
    (when (and (> depth 0) (eq (car-safe form) 'use-package))
      (error "%s: `use-package' for `%s' is not at top level"
             (file-name-nondirectory file) (cadr form)))
    (let ((tail form))
      (while (consp tail)
        (ginshio-maintain--assert-top-level (car tail) file (1+ depth))
        (setq tail (cdr tail))))))

(defun ginshio-maintain--file-declarations (file)
  "Return the (PACKAGE . RECIPE) cells FILE declares, in source order."
  (let (cells)
    (dolist (form (ginshio-maintain--read-forms file))
      (ginshio-maintain--assert-top-level form file 0)
      (when (eq (car-safe form) 'use-package)
        (let ((name (cadr form)))
          ;; The recipe is never evaluated, here or at startup: `use-package'
          ;; hands the normalizer the source form, and both sides pass it to
          ;; straight as data.  So it is read exactly as written, and the only
          ;; way the two can disagree is a declaration one of them cannot see.
          (dolist (value (ginshio-maintain--straight-args
                          (cddr form) name file))
            (dolist (recipe (ginshio-manifest-normalize name value))
              (push (cons (if (consp recipe) (car recipe) recipe) recipe)
                    cells))))))
    (nreverse cells)))

(defun ginshio-maintain--module-files ()
  "Return the tangled module files in load order.
`ginshio-modules' is generated by the tangle, so the order this tool
walks is the order startup loads; no second list is maintained."
  (let* ((init (expand-file-name "ginshio-init.el" ginshio-generated-lisp-dir))
         (form (cl-find-if (lambda (f)
                             (and (eq (car-safe f) 'defvar)
                                  (eq (cadr f) 'ginshio-modules)))
                           (ginshio-maintain--read-forms init))))
    (unless form
      (error "No `ginshio-modules' declaration in %s" init))
    (let ((modules (nth 2 form)))
      (when (eq (car-safe modules) 'quote)
        (setq modules (cadr modules)))
      (mapcar (lambda (module)
                (expand-file-name (format "ginshio-%s.el" module)
                                  ginshio-generated-lisp-dir))
              modules))))

(defun ginshio-maintain--collect ()
  "Return every declared (PACKAGE . RECIPE) cell, in load order."
  (let* ((ordered (ginshio-maintain--module-files))
         (cells nil))
    (dolist (file ordered)
      (unless (file-readable-p file)
        (error "Module %s is missing; tangle the configuration first" file))
      (dolist (cell (ginshio-maintain--file-declarations file))
        (unless (member cell cells)
          (push cell cells))))
    ;; A module outside `ginshio-modules' is a support file that startup only
    ;; reaches through `require'.  It may not declare packages: this tool would
    ;; not see the declaration, and the snapshot would be short one package.
    (dolist (file (directory-files ginshio-generated-lisp-dir t "\\.el\\'" t))
      (unless (member file ordered)
        (when (ginshio-maintain--file-declarations file)
          (error "%s declares packages but is not a runtime module"
                 (file-name-nondirectory file)))))
    (nreverse cells)))

(defvar ginshio-maintain--declarations nil
  "Declared (PACKAGE . RECIPE) cells, in load order.")


;;;; straight

(defvar straight-base-dir ginshio-data-dir
  "Parent directory of the package tree; straight adds `straight/'.")

;; A build directory per Emacs version.  Byte code is not portable across
;; versions, and straight only rebuilds when a recipe changes or a build is
;; absent -- never because the running Emacs changed.  Keying the tree by
;; version turns that from a rebuild the user has to remember into an empty
;; build cache the next sync fills, and lets two versions share one set of
;; clones and one lock.  `straight--build-cache-file' appends `-cache.el' to
;; this name, so a trailing slash would place the cache inside the tree
;; `straight-prune-build' walks.
(defvar straight-build-dir (concat "build-" emacs-version))

(defvar straight-profiles `((nil . ,ginshio-manifest-lock-file))
  "Track straight's lockfile at the repository `straight.lock'.")
(defvar straight-check-for-modifications nil
  "Never scan repositories; this tool decides what to rebuild.")
(defvar straight-vc-git-default-clone-depth 1
  "Clone shallowly; straight deepens when a pinned commit needs it.")
(defvar straight-vc-git-default-protocol 'https)
(defvar straight-repository-branch "develop")

(defun ginshio-maintain--load-straight ()
  "Load straight.el, bootstrapping it from the network exactly once."
  (let ((bootstrap (expand-file-name "straight/repos/straight.el/bootstrap.el"
                                     straight-base-dir)))
    (unless (file-exists-p bootstrap)
      (let ((install (expand-file-name "straight/repos/straight.el/install.el"
                                       straight-base-dir)))
        (unless (file-exists-p install)
          (with-current-buffer
              (url-retrieve-synchronously
               "https://raw.githubusercontent.com/radian-software/straight.el/develop/install.el"
               'silent 'inhibit-cookies)
            (goto-char (point-max))
            (eval-print-last-sexp)))
        (load install nil 'nomessage)))
    (load bootstrap nil 'nomessage))
  (require 'straight)
  ;; Packages are byte-compiled here; native compilation is left to the
  ;; sessions that actually load them.
  (setq native-comp-jit-compilation nil))


;;;; Resolving, checking out and rebuilding

(defun ginshio-maintain--resolve (rebuild)
  "Register, clone and build every declaration.
REBUILD is `straight--packages-to-rebuild': nil, a hash table of package
names, or `:all'.

Each pass opens its own straight transaction.  `straight-use-package'
skips a recipe it has already seen within a transaction, and in batch a
transaction lasts until Emacs exits -- so a second pass over the same
declarations would do nothing at all unless the first one is closed."
  (straight--transaction-finalize)
  (let ((straight--packages-to-rebuild rebuild)
        (straight--packages-not-to-rebuild (make-hash-table :test #'equal)))
    (dolist (cell ginshio-maintain--declarations)
      ;; straight is handed a copy because `straight--convert-recipe' fills a
      ;; recipe out with its defaults through `plist-put', which appends onto
      ;; the very list it was passed.  Resolving the live cell would write a
      ;; straight-expanded recipe into the snapshot, and startup -- which
      ;; records the recipe as written -- could never reproduce it.
      (straight-use-package (copy-tree (cdr cell))))))

(defun ginshio-maintain--revisions ()
  "Return (LOCAL-REPO . COMMIT) for every repository in the profile, sorted."
  (let (revisions)
    (straight--map-repos
     (lambda (recipe)
       (straight--with-plist recipe
           (package local-repo type)
         (when (and local-repo
                    (memq nil (gethash package straight--profile-cache))
                    (straight--repository-is-available-p recipe))
           (when-let* ((commit (straight-vc-get-commit type local-repo)))
             (push (cons local-repo commit) revisions))))))
    (cl-sort revisions #'string-lessp :key #'car)))

(defun ginshio-maintain--moved (before)
  "Return the local repositories whose revision differs from the BEFORE alist.
A repository cloned since BEFORE was taken counts as moved."
  (let (moved)
    (dolist (cell (ginshio-maintain--revisions) moved)
      (unless (equal (cdr cell) (cdr (assoc (car cell) before)))
        (push (car cell) moved)))))

(defun ginshio-maintain--rebuild-table (repos)
  "Return a rebuild hash table naming every package built from local REPOS.
One repository can serve several packages, so the mapping is taken from
the recipe cache rather than assumed to be one to one."
  (let ((table (make-hash-table :test #'equal)))
    (maphash (lambda (package recipe)
               (when (member (plist-get recipe :local-repo) repos)
                 (puthash package t table)))
             straight--recipe-cache)
    table))

(defun ginshio-maintain--thaw ()
  "Check out the locked revision wherever HEAD differs.
Return the local repositories that moved.

`straight-thaw-versions' calls `straight-vc-check-out-commit' for every
repository unconditionally, and that path normalizes remotes and branches
before it compares anything.  Normalization asks git for the remote's
default branch, which costs a network round trip per repository whenever
the ref is not cached locally.  Comparing first keeps a synchronization
that changes nothing entirely offline."
  (let ((locked (straight--lockfile-read-all))
        moved)
    (straight--map-repos
     (lambda (recipe)
       (straight--with-plist recipe
           (local-repo type)
         (when (and local-repo (straight--repository-is-available-p recipe))
           (let ((commit (cdr (assoc local-repo locked))))
             (when (and commit
                        (not (equal commit
                                    (straight-vc-get-commit type local-repo))))
               (unless (straight-vc-commit-present-p recipe commit)
                 (straight-vc-fetch-from-remote recipe))
               (straight-vc-check-out-commit recipe commit)
               (push local-repo moved)))))))
    moved))

(defun ginshio-maintain--loose-ref (directory ref)
  "Return the contents of REF's loose file below DIRECTORY, or nil."
  (let ((file (expand-file-name (concat ".git/" ref) directory)))
    (when (file-readable-p file)
      (with-temp-buffer
        (insert-file-contents file)
        (string-trim (buffer-string))))))

(defun ginshio-maintain--ref-exists-p (directory ref)
  "Return non-nil when REF exists below DIRECTORY, loose or packed."
  (or (and (ginshio-maintain--loose-ref directory ref) t)
      (let ((packed (expand-file-name ".git/packed-refs" directory)))
        (when (file-readable-p packed)
          (with-temp-buffer
            (insert-file-contents packed)
            (goto-char (point-min))
            (and (re-search-forward
                  (concat "^[0-9a-f]+ " (regexp-quote ref) "$") nil t)
                 t))))))

(defun ginshio-maintain--remote-head-resolves-p (directory ref)
  "Return non-nil when REF is a symbolic ref whose target exists.
Answered from the files alone, because this runs once per repository on
every maintenance command.  A symbolic ref is always loose, so a missing
file is a missing ref; its target may have been packed by `git gc'.
straight's own bootstrap leaves this ref dangling, so the ref existing is
not on its own enough to go on."
  (when-let* ((head (ginshio-maintain--loose-ref directory ref))
              (target (and (string-prefix-p "ref: " head)
                           (substring head 5))))
    (ginshio-maintain--ref-exists-p directory target)))

(defun ginshio-maintain--adopt-default-branch ()
  "Give each repository the REMOTE/HEAD ref straight looks for, locally.

`straight-vc-git--default-remote-branch' reads the default branch out of
`git branch -r' and, when it cannot find it there, falls back to `git
remote show' -- which contacts the server.  A shallow clone arrives with
no remote-tracking refs at all, so `git branch -r' is empty for every
repository and that fallback fires on every normalizing checkout, fetch
and merge: one network round trip per repository, per operation, enough
of them that a forge starts answering 429.

Two refs are written, both from what is already on disk.  The
remote-tracking ref is set to the commit the local branch is on, which is
an assertion about this clone rather than about the remote; the first
real fetch overwrites it through the refspec the clone already
configures, and until then it can only report the branch as up to date --
the same conclusion a maintenance run reaches when it does not fetch.
The branch adopted is the one checked out, which is the default branch
whenever straight had to ask for it: a recipe carrying an explicit
`:branch' is used directly and never consults this ref at all."
  (let ((adopted 0))
    (straight--map-repos
     (lambda (recipe)
       (straight--with-plist recipe
           (local-repo type remote)
         (when (and local-repo (eq type 'git)
                    (straight--repository-is-available-p recipe))
           (let* ((remote (or remote straight-vc-git-default-remote-name))
                  (head-ref (format "refs/remotes/%s/HEAD" remote))
                  (directory (straight--repos-dir local-repo)))
             (unless (ginshio-maintain--remote-head-resolves-p
                      directory head-ref)
               (let* ((straight--default-directory directory)
                      (branch (straight--process-output
                               "git" "rev-parse" "--abbrev-ref" "HEAD")))
                 ;; A detached HEAD names no branch to adopt.
                 (unless (equal branch "HEAD")
                   (let ((target (format "refs/remotes/%s/%s" remote branch)))
                     ;; `rev-parse' rather than the loose file: a repacked ref
                     ;; is real, and overwriting it would throw away what a
                     ;; fetch learned from the remote.
                     (unless (straight--process-run-p
                              "git" "rev-parse" "--verify" "--quiet" target)
                       (straight--process-run-p
                        "git" "update-ref" target
                        (straight--process-output "git" "rev-parse" "HEAD")))
                     (straight--process-run-p
                      "git" "symbolic-ref" head-ref target)
                     (cl-incf adopted))))))))))
    (unless (zerop adopted)
      (straight--output "Recorded the default branch of %d repositories"
                        adopted))))


;;;; The lock

(defun ginshio-maintain--write-lock (revisions)
  "Replace the profile lockfile with REVISIONS.
Keeps the format `straight-freeze-versions' produces: one (LOCAL-REPO
. COMMIT) cell per line, sorted by repository."
  (let* ((path (straight--versions-file (cdr (assq nil straight-profiles))))
         (candidate (make-temp-file
                     (expand-file-name ".straight-lock-"
                                       (file-name-directory path)))))
    (unwind-protect
        (progn
          (with-temp-file candidate
            (insert (format "(%s)\n:epsilon\n"
                            (mapconcat (apply-partially #'format "%S")
                                       revisions "\n "))))
          (rename-file candidate path t)
          (set-file-modes path #o644)
          (straight--output "Wrote %s" path))
      (when (file-exists-p candidate)
        (delete-file candidate)))))

(defun ginshio-maintain--reconcile-lock ()
  "Keep every current pin, add repositories new since the last lock, drop the rest."
  (let ((locked (straight--lockfile-read-all))
        current)
    (dolist (cell (ginshio-maintain--revisions))
      (push (cons (car cell) (or (cdr (assoc (car cell) locked)) (cdr cell)))
            current))
    (ginshio-maintain--write-lock
     (cl-sort current #'string-lessp :key #'car))))


;;;; The activation snapshot

(defun ginshio-maintain--profile-packages ()
  "Return the packages in the current straight profile, sorted."
  (let (packages)
    (maphash (lambda (package profiles)
               (when (memq nil profiles)
                 (push (intern package) packages)))
             straight--profile-cache)
    (sort packages (lambda (left right)
                     (string< (symbol-name left) (symbol-name right))))))

(defun ginshio-maintain--build-directories (packages)
  "Return the build directories of PACKAGES, in `load-path' order."
  (let ((root (file-name-as-directory (straight--build-dir)))
        (allowed (make-hash-table :test #'equal))
        ordered)
    (dolist (package packages)
      (puthash (symbol-name package) t allowed))
    (dolist (path load-path)
      (let ((directory (directory-file-name (expand-file-name path))))
        (when (and (string-prefix-p root (file-name-as-directory directory))
                   (gethash (file-name-nondirectory directory) allowed)
                   (file-directory-p directory)
                   (not (member directory ordered)))
          (push directory ordered))))
    (setq ordered (nreverse ordered))
    ;; Resolving puts every buildable package on `load-path', so filtering it
    ;; is complete and preserves straight's dependency order.  Assert that
    ;; rather than collecting the leftovers: a package with a build directory
    ;; that never reached `load-path' would otherwise drop out of the snapshot
    ;; silently, and only fail much later as a missing feature.
    (dolist (package packages)
      (let ((directory (directory-file-name
                        (straight--build-dir (symbol-name package)))))
        (when (and (file-directory-p directory)
                   (not (member directory ordered)))
          (error "Package `%s' was built but never reached load-path" package))))
    ordered))

(defun ginshio-maintain--autoload-forms (directories)
  "Return forms activating the autoloads found in DIRECTORIES."
  (let (forms)
    (dolist (directory directories (nreverse forms))
      (when-let* ((files (directory-files
                          directory t "\\(?:-autoloads\\)\\.el\\'" t))
                  (file (car files)))
        (push `(let ((load-file-name ,file)
                     (load-in-progress t)
                     (current-load-list nil))
                 ,@(ginshio-maintain--read-forms file))
              forms)))))

(defun ginshio-maintain--write-activation ()
  "Regenerate and atomically replace the activation snapshot."
  (let* ((packages (ginshio-maintain--profile-packages))
         (builds (ginshio-maintain--build-directories packages))
         (autoloads (ginshio-maintain--autoload-forms builds))
         (candidate (make-temp-file
                     (expand-file-name ".activation-" ginshio-data-dir)
                     nil ".el"))
         (candidate-compiled (byte-compile-dest-file candidate)))
    (unwind-protect
        (progn
          (with-temp-file candidate
            (insert ";;; activation.el --- generated package snapshot"
                    " -*- lexical-binding: t; -*-\n\n")
            (dolist (form
                     (list `(setq ginshio-manifest-lock-hash
                                  ,(ginshio-manifest-lock-digest))
                           `(setq ginshio-manifest-packages ',packages)
                           `(setq ginshio-manifest-declarations
                                  ',ginshio-maintain--declarations)
                           `(setq load-path (append ',builds load-path))))
              (prin1 form (current-buffer))
              (insert "\n"))
            (dolist (form autoloads)
              (prin1 form (current-buffer))
              (insert "\n")))
          ;; Generated autoloads produce warnings with no actionable source
          ;; location in this configuration.
          (let ((byte-compile-warnings nil)
                (byte-compile-log-warning-function #'ignore))
            (byte-compile-file candidate))
          (unless (file-readable-p candidate-compiled)
            (error "Failed to compile the package snapshot %s" candidate))
          (rename-file candidate ginshio-manifest-source t)
          (rename-file candidate-compiled ginshio-manifest-file t)
          (straight--output "Wrote %s" ginshio-manifest-file))
      (when (file-exists-p candidate)
        (delete-file candidate))
      (when (file-exists-p candidate-compiled)
        (delete-file candidate-compiled)))))

(defun ginshio-maintain--publish ()
  "Write the snapshot, then drop repositories and builds nothing declares."
  (ginshio-maintain--write-activation)
  (straight-remove-unused-repos 'force)
  (straight-prune-build))


;;;; Commands

(defconst ginshio-maintain--thaw-rounds 4
  "How many times a sync may re-resolve before giving up.
Checking out a locked revision can expose a dependency the previous
revision did not have, which has to be cloned and can itself be locked
to a revision with different dependencies.  The fixed point is normally
reached in two rounds; more than this means the lock and the recipes
disagree in a way no further round will settle.")

(defun ginshio-maintain--sync ()
  "Install what the configuration declares, at the revisions the lock pins."
  (ginshio-maintain--resolve nil)
  (ginshio-maintain--adopt-default-branch)
  (let ((moved (ginshio-maintain--thaw))
        (round 0))
    (while moved
      (when (> (cl-incf round) ginshio-maintain--thaw-rounds)
        (error "Package graph did not settle after %d rounds"
               ginshio-maintain--thaw-rounds))
      (ginshio-maintain--resolve (ginshio-maintain--rebuild-table moved))
      (setq moved (ginshio-maintain--thaw))))
  (ginshio-maintain--reconcile-lock)
  (ginshio-maintain--publish))

(defun ginshio-maintain--upgrade ()
  "Fetch upstream, rebuild what moved, and pin the new revisions."
  (ginshio-maintain--resolve nil)
  (ginshio-maintain--adopt-default-branch)
  (let ((before (ginshio-maintain--revisions)))
    (straight-pull-all)
    ;; Resolving again discovers dependencies from the upgraded sources
    ;; before the new lock is published.
    (ginshio-maintain--resolve
     (ginshio-maintain--rebuild-table (ginshio-maintain--moved before))))
  (ginshio-maintain--write-lock (ginshio-maintain--revisions))
  (ginshio-maintain--publish))

(defun ginshio-maintain--rebuild ()
  "Rebuild every package at its locked revision.
The sledgehammer: it takes straight's normalizing checkout rather than
the comparison `ginshio-maintain--thaw' uses, so a repository left on the
wrong branch or with a stale remote is put right here.  It also covers
the build this tool cannot detect as stale -- one interrupted halfway, or
one whose dependency changed a macro it had already inlined."
  (ginshio-maintain--resolve nil)
  (ginshio-maintain--adopt-default-branch)
  (straight-thaw-versions)
  (ginshio-maintain--resolve :all)
  (ginshio-maintain--publish))

(defun ginshio-maintain (command)
  "Perform COMMAND, one of `ginshio-maintain-commands'."
  (unless (member command ginshio-maintain-commands)
    (error "Unknown maintenance command: %s" command))
  (ginshio-maintain--load-straight)
  (setq ginshio-maintain--declarations (ginshio-maintain--collect))
  (straight--output "Read %d package declarations"
                    (length ginshio-maintain--declarations))
  (pcase command
    ("sync" (ginshio-maintain--sync))
    ("upgrade" (ginshio-maintain--upgrade))
    ("rebuild" (ginshio-maintain--rebuild)))
  (straight--output "Package graph is current"))

(defvar ginshio-maintain-auto-run t
  "When non-nil, loading this file in batch mode runs the command in argv.")

(when (and noninteractive ginshio-maintain-auto-run)
  (ginshio-maintain (pop command-line-args-left)))

;;; maintain.el ends here
