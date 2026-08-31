;;; early-init.el --- fail-closed bootstrap loader -*- lexical-binding: t; -*-

(defvar ginshio-early-init-complete nil)

(let ((payload
       (expand-file-name
        "modules/ginshio/ginshio-early-init.el" user-emacs-directory)))
  (condition-case err
      (progn
        (load payload nil 'nomessage t)
        (unless ginshio-early-init-complete
          (error "Generated early-init payload did not complete")))
    (error
     (if init-file-debug
         (signal (car err) (cdr err))
       (let ((reason (concat (error-message-string err)
                             "  Start again with --debug-init for a backtrace.")))
         ;; A desktop launch has no stderr to read and the echo area dies
         ;; with the process: put the reason where it can actually be read.
         ;; A dialog waits for dismissal, a terminal holds the message for a
         ;; few seconds, batch keeps the plain stderr line.
         (message "Ginshio early init failed: %s" reason)
         (condition-case nil
             (cond ((and (display-graphic-p) (not noninteractive))
                    (message-box "Ginshio early init failed: %s" reason))
                   ((and (not noninteractive) (not (daemonp)))
                    (sit-for 4)))
           (error nil))
         (kill-emacs 1))))))

;;; early-init.el ends here
