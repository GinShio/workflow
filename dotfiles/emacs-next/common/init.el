;;; init.el --- fail-closed bootstrap loader -*- lexical-binding: t; -*-

(defvar ginshio-init-complete nil)

(let ((payload
       (expand-file-name "modules/ginshio/ginshio-init.el" user-emacs-directory)))
  (condition-case err
      (progn
        (load payload nil 'nomessage t)
        (unless ginshio-init-complete
          (error "Generated init payload did not complete")))
    (error
     (if init-file-debug
         (signal (car err) (cdr err))
       (message "Ginshio init failed: %s" (error-message-string err))
       (kill-emacs 1)))))

;;; init.el ends here
