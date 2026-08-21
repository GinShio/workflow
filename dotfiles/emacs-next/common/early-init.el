;;; early-init.el --- fail-closed bootstrap loader -*- lexical-binding: t; -*-

(defvar ginshio-early-init-complete nil)

(let ((payload
       (expand-file-name
        "lisp/ginshio/ginshio-early-init.el" user-emacs-directory)))
  (condition-case err
      (progn
        (load payload nil 'nomessage t)
        (unless ginshio-early-init-complete
          (error "Generated early-init payload did not complete")))
    (error
     (if init-file-debug
         (signal (car err) (cdr err))
       (message "Ginshio early init failed: %s" (error-message-string err))
       (kill-emacs 1)))))

;;; early-init.el ends here
