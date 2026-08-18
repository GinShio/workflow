;;; tangle.el --- Tangle config.org with #+INCLUDE expansion -*- lexical-binding: t; -*-

;; Usage:
;;   emacs --batch -l tangle.el
;;   emacs --batch -l tangle.el --eval '(ginshio-tangle-config "path/to/config.org")'

;;; Commentary:

;; `org-babel-tangle-file' does NOT expand #+INCLUDE directives, as those are
;; part of the export framework (ox.el).  The configuration is split across
;; org/*.org and stitched together by config.org, so includes must be expanded
;; before tangling or every module would be silently dropped.

;;; Code:

(require 'org)
(require 'ob-tangle)
(require 'ox)

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
  "Tangle FILE (or config.org beside this script) with #+INCLUDE expanded."
  (let* ((file (or file
                   (expand-file-name "config.org"
                                     (if load-file-name
                                         (file-name-directory load-file-name)
                                       default-directory))))
         (org-confirm-babel-evaluate nil)
         (temp-file (make-temp-file "ginshio-tangle-" nil ".org")))
    (message "Tangling %s ..." file)
    (unwind-protect
        (progn
          (with-temp-buffer
            (insert-file-contents file)
            (org-mode)
            (setq default-directory (file-name-directory file))
            (org-export-expand-include-keyword)
            (ginshio-tangle--ensure-target-directories)
            (write-region (point-min) (point-max) temp-file nil 'silent))
          (with-current-buffer (find-file-noselect temp-file)
            ;; Tangling reads the buffer's file name back: `:comments link'
            ;; puts it into the generated headers, and `org-babel' visits the
            ;; file again while collecting blocks.  It has to name config.org,
            ;; not the scratch copy.  Claiming the name alone is not enough --
            ;; buffers are looked up by truename, and the modtime recorded
            ;; when the scratch copy was read belongs to a different file.
            ;; Leave either stale and Org either re-reads config.org with its
            ;; includes unexpanded, or stops to ask whether it should, which
            ;; in batch is an end-of-file error on stdin.
            (setq default-directory (file-name-directory file)
                  buffer-file-name file
                  buffer-file-truename (file-truename file))
            (set-visited-file-modtime)
            (org-mode-restart)
            (org-babel-tangle))
          (message "Tangling complete, %s" file))
      (when (file-exists-p temp-file)
        (delete-file temp-file)))))

(when noninteractive
  (ginshio-tangle-config))

;;; tangle.el ends here
