;;; tangle.el --- Tangle the discovered Org modules -*- lexical-binding: t; -*-

;; Usage:
;;   emacs --batch -l tangle.el
;;   emacs --batch --eval '(setq ginshio-tangle-auto-run nil)' -l tangle.el \
;;     --eval '(ginshio-tangle-config "path/to/config.org")'

;;; Commentary:

;; `org-babel-tangle-file' does NOT expand #+INCLUDE directives, as those are
;; part of the export framework (ox.el).  Module sources are discovered and
;; sorted by filename, then inserted as includes before expansion and tangling.

;;; Code:

(require 'org)
(require 'ob-tangle)
(require 'ox)
(require 'cl-lib)
(require 'subr-x)

(defconst ginshio-tangle--source-name-regexp
  "\\`[0-9][0-9]-\\([a-z][a-z0-9-]*\\)\\.org\\'")

(defconst ginshio-tangle--manifest-marker-regexp
  "^[ \t]*#\\+GINSHIO_MODULES:[ \t]*$")

(defconst ginshio-tangle--module-directory "lisp/ginshio/"
  "Directory below the configuration root containing tangled modules.")

(defconst ginshio-tangle--payload-targets
  '("lisp/ginshio/ginshio-early-init.el"
    "lisp/ginshio/ginshio-init.el")
  "Generated payloads loaded by the tracked root bootstrap files.")

(cl-defstruct
    (ginshio-tangle-source (:constructor ginshio-tangle--make-source))
  file name state)

(defvar ginshio-tangle-auto-run t
  "When non-nil, loading this file in batch mode tangles the default config.")

(defun ginshio-tangle--module-state (file)
  "Return FILE's single `GINSHIO_MODULE' metadata value."
  (with-temp-buffer
    (insert-file-contents file)
    (setq default-directory (file-name-directory file))
    (org-mode)
    (let* ((keywords (org-collect-keywords '("GINSHIO_MODULE")))
           (values (cdr (assoc "GINSHIO_MODULE" keywords))))
      (unless (= (length values) 1)
        (error "%s: expected exactly one #+GINSHIO_MODULE value" file))
      (let ((state (intern (string-trim (car values)))))
        (unless (memq state '(bootstrap support enabled disabled))
          (error
           "%s: #+GINSHIO_MODULE must be bootstrap, support, enabled, or disabled"
           file))
        state))))

(defun ginshio-tangle--parse-source (file)
  "Return the module source represented by Org FILE."
  (let ((basename (file-name-nondirectory file)))
    (unless (string-match ginshio-tangle--source-name-regexp basename)
      (error "%s: source name must match NN-name.org" file))
    (ginshio-tangle--make-source
     :file file
     :name (intern (match-string 1 basename))
     :state (ginshio-tangle--module-state file))))

(defun ginshio-tangle--discover-sources (directory)
  "Return Org sources below DIRECTORY in deterministic filename order."
  (let ((org-directory (expand-file-name "org/" directory)))
    (unless (file-directory-p org-directory)
      (error "Module source directory does not exist: %s" org-directory))
    (let ((files
           (directory-files
            org-directory t ginshio-tangle--source-name-regexp t)))
      (unless files
        (error "No module sources found in %s" org-directory))
      (mapcar
       #'ginshio-tangle--parse-source
       (sort files
             (lambda (left right)
               (string< (file-name-nondirectory left)
                        (file-name-nondirectory right))))))))

(defun ginshio-tangle--validate-sources (sources)
  "Validate the naming and bootstrap contract of sorted SOURCES."
  (let ((names (make-hash-table :test #'eq))
        bootstrap)
    (dolist (source sources)
      (let ((name (ginshio-tangle-source-name source)))
        (when (gethash name names)
          (error "Module name %s is declared by both %s and %s"
                 name
                 (ginshio-tangle-source-file (gethash name names))
                 (ginshio-tangle-source-file source)))
        (puthash name source names))
      (when (eq (ginshio-tangle-source-state source) 'bootstrap)
        (when bootstrap
          (error "Bootstrap is declared by both %s and %s"
                 (ginshio-tangle-source-file bootstrap)
                 (ginshio-tangle-source-file source)))
        (setq bootstrap source)))
    (unless bootstrap
      (error "No bootstrap source declared"))
    (unless (eq (car sources) bootstrap)
      (error "Bootstrap source must sort before every other source"))
    sources))

(defun ginshio-tangle--included-p (source)
  "Return non-nil when SOURCE participates in this tangle."
  (not (eq (ginshio-tangle-source-state source) 'disabled)))

(defun ginshio-tangle--module-list (sources)
  "Return enabled runtime module names from sorted SOURCES."
  (cl-loop for source in sources
           when (eq (ginshio-tangle-source-state source) 'enabled)
           collect (ginshio-tangle-source-name source)))

(defun ginshio-tangle--render-manifest (sources directory)
  "Render the generated module block and includes for SOURCES below DIRECTORY."
  (let ((modules (ginshio-tangle--module-list sources))
        (included (cl-remove-if-not #'ginshio-tangle--included-p sources)))
    (concat
     "#+name: ginshio-generated-modules\n"
     "#+begin_src emacs-lisp :tangle no\n"
     (format "(%s)\n" (mapconcat #'symbol-name modules " "))
     "#+end_src\n\n"
     (mapconcat
      (lambda (source)
        (format "#+include: %S"
                (file-relative-name
                 (ginshio-tangle-source-file source) directory)))
      included
      "\n"))))

(defun ginshio-tangle--inject-manifest (sources directory)
  "Replace the current buffer's manifest marker for SOURCES below DIRECTORY."
  (let ((case-fold-search t)
        beginning
        end)
    (goto-char (point-min))
    (unless (re-search-forward ginshio-tangle--manifest-marker-regexp nil t)
      (error "Missing #+GINSHIO_MODULES: marker"))
    (setq beginning (match-beginning 0)
          end (match-end 0))
    (when (re-search-forward ginshio-tangle--manifest-marker-regexp nil t)
      (error "More than one #+GINSHIO_MODULES: marker"))
    (delete-region beginning end)
    (goto-char beginning)
    (insert (ginshio-tangle--render-manifest sources directory))))

(defun ginshio-tangle--expected-targets (sources directory)
  "Return every file the tangle must produce below DIRECTORY."
  (append
   (mapcar (lambda (target) (expand-file-name target directory))
           ginshio-tangle--payload-targets)
   (cl-loop
    for source in sources
    when (ginshio-tangle--included-p source)
    collect
    (expand-file-name
     (concat
      ginshio-tangle--module-directory
      (if (eq (ginshio-tangle-source-state source) 'bootstrap)
          "ginshio-path.el"
        (format "ginshio-%s.el" (ginshio-tangle-source-name source))))
     directory))))

(defun ginshio-tangle--validate-elisp (file)
  "Read every form in FILE, signalling on malformed generated Lisp."
  (with-temp-buffer
    (insert-file-contents file)
    (emacs-lisp-mode)
    (check-parens)
    (goto-char (point-min))
    (condition-case err
        (while t (read (current-buffer)))
      (end-of-file t)
      (error
       (error "Generated Lisp is invalid in %s: %s"
              file (error-message-string err))))))

(defun ginshio-tangle--redirect-targets (directory)
  "Rewrite relative :tangle targets in this buffer below DIRECTORY."
  (save-excursion
    (goto-char (point-min))
    (let ((case-fold-search t))
      (while
          (re-search-forward
           "\\(:tangle[ \t]+\\)\"?\\([^ \t\"\n]+\\)\"?" nil t)
        (let ((target (match-string 2)))
          (unless (member target '("no" "yes"))
            (replace-match
             (concat (match-string 1)
                     (prin1-to-string (expand-file-name target directory)))
             t t)))))))

(defun ginshio-tangle--promote (staging directory)
  "Atomically replace the generated subtree in DIRECTORY from STAGING."
  (let* ((relative (directory-file-name ginshio-tangle--module-directory))
         (candidate (expand-file-name relative staging))
         (target (expand-file-name relative directory))
         (backup (concat target ".previous")))
    (unless (file-directory-p candidate)
      (error "Tangle staging subtree is missing: %s" candidate))
    (when (file-exists-p backup)
      (delete-directory backup t))
    (when (file-exists-p target)
      (rename-file target backup))
    (condition-case err
        (progn
          (rename-file candidate target)
          (when (file-exists-p backup)
            (delete-directory backup t)))
      (error
       (when (and (not (file-exists-p target))
                  (file-exists-p backup))
         (rename-file backup target))
       (signal (car err) (cdr err))))))

(defun ginshio-tangle--ensure-target-directories ()
  "Create directories named by :tangle headers that do not exist yet.
`org-babel-tangle' opens its targets with `write-region', which fails rather
than creating intermediate directories."
  (save-excursion
    (goto-char (point-min))
    (let ((case-fold-search t))
      (while (re-search-forward ":tangle[ \t]+\"?\\([^ \t\"\n]+\\)" nil t)
        (let ((target (match-string 1)))
          (unless (member target '("no" "yes"))
            (let ((dir (file-name-directory (expand-file-name target))))
              (when (and dir (not (file-directory-p dir)))
                (make-directory dir t)))))))))

(defun ginshio-tangle-config (&optional file)
  "Tangle FILE (or config.org beside this script) from discovered modules."
  (let* ((file
          (expand-file-name
           (or file
               (expand-file-name
                "config.org"
                (if load-file-name
                    (file-name-directory load-file-name)
                  default-directory)))))
         (directory (file-name-directory file))
         (sources
          (ginshio-tangle--validate-sources
           (ginshio-tangle--discover-sources directory)))
         (staging
          (make-temp-file
           (expand-file-name ".ginshio-tangle-" directory) t))
         (expected-targets
          (ginshio-tangle--expected-targets sources staging))
         tangled-targets
         (org-confirm-babel-evaluate nil)
         (temp-file (make-temp-file "ginshio-tangle-" nil ".org")))
    (message "Tangling %s ..." file)
    (unwind-protect
        (progn
          (with-temp-buffer
            (insert-file-contents file)
            (org-mode)
            (setq default-directory directory)
            (ginshio-tangle--inject-manifest sources directory)
            (org-export-expand-include-keyword)
            (ginshio-tangle--redirect-targets staging)
            (write-region (point-min) (point-max) temp-file nil 'silent))
          (let ((tangle-buffer (find-file-noselect temp-file)))
            (unwind-protect
                (with-current-buffer tangle-buffer
                  ;; Tangling reads the buffer's file name back: `:comments link'
                  ;; puts it into the generated headers, and `org-babel' visits the
                  ;; file again while collecting blocks.  It has to name config.org,
                  ;; not the scratch copy.  Claiming the name alone is not enough --
                  ;; buffers are looked up by truename, and the modtime recorded
                  ;; when the scratch copy was read belongs to a different file.
                  ;; Leave either stale and Org either re-reads config.org with its
                  ;; includes unexpanded, or stops to ask whether it should, which
                  ;; in batch is an end-of-file error on stdin.
                  (setq default-directory staging
                        buffer-file-name file
                        buffer-file-truename (file-truename file))
                  (set-visited-file-modtime)
                  (org-mode-restart)
                  (ginshio-tangle--ensure-target-directories)
                  (setq tangled-targets
                        (mapcar
                         (lambda (target)
                           (expand-file-name target staging))
                         (org-babel-tangle))))
              (when (buffer-live-p tangle-buffer)
                (with-current-buffer tangle-buffer
                  (set-buffer-modified-p nil))
                (kill-buffer tangle-buffer))))
          (dolist (target expected-targets)
            (unless (and (member target tangled-targets)
                         (file-exists-p target))
              (error "Tangle did not produce expected target %s" target))
            (ginshio-tangle--validate-elisp target))
          (ginshio-tangle--promote staging directory)
          (message "Tangling complete, %s" file))
      (when (file-exists-p temp-file)
        (delete-file temp-file))
      (when (file-directory-p staging)
        (delete-directory staging t)))))

(when (and noninteractive ginshio-tangle-auto-run)
  (ginshio-tangle-config))

;;; tangle.el ends here
