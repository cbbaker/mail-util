;;; mail-util.el --- Review and apply mail-sorting suggestions -*- lexical-binding: t; -*-

;; Author: mail-util contributors
;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: mail

;;; Commentary:

;; An Emacs front-end for the `mail-util' CLI.  It runs the (read-only)
;; `mail-util suggest' command, presents the proposed sorting folders in a
;; review buffer where each cluster can be approved or rejected, and exports the
;; approved set to JSON.  Creating folders / moving mail (the `apply' command)
;; is not implemented in the CLI yet; the review + export workflow lets you try
;; the analysis today and grows into the full loop as the CLI gains `plan' and
;; `apply'.

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'subr-x)
(require 'text-property-search)

(defgroup mail-util nil
  "Review and apply mail-sorting suggestions from the mail-util CLI."
  :group 'mail
  :prefix "mail-util-")

(defcustom mail-util-executable "mail-util"
  "Path to the mail-util command-line binary.
If mail-util is not on `exec-path', set this to the absolute path,
e.g. \"~/src/mail-util/target/release/mail-util\"."
  :type 'string)

(defcustom mail-util-root nil
  "Account root: the Maildir directory whose subdirectories are the folders.
When nil, the CLI falls back to the MAILUTIL_ROOT environment variable."
  :type '(choice (const :tag "Use MAILUTIL_ROOT env" nil) directory))

(defcustom mail-util-inbox ".INBOX"
  "Inbox folder to analyze, relative to the account root."
  :type 'string)

(defcustom mail-util-min-count 30
  "Only surface clusters with at least this many messages."
  :type 'natnum)

(defcustom mail-util-export-file
  (expand-file-name "mail-util-approved.json" user-emacs-directory)
  "Default file for `mail-util-export-approved'."
  :type 'file)

;;;; Faces

(defface mail-util-approved '((t :inherit success))
  "Face for an approved cluster.")

(defface mail-util-rejected '((t :inherit shadow :strike-through t))
  "Face for a rejected cluster.")

(defface mail-util-count '((t :inherit font-lock-constant-face))
  "Face for the message-count column.")

(defface mail-util-destination '((t :inherit font-lock-function-name-face))
  "Face for the proposed destination folder.")

(defface mail-util-key '((t :inherit shadow))
  "Face for the cluster key.")

;;;; Buffer-local state

(defvar-local mail-util--clusters nil
  "Vector of cluster alists from the last `suggest' run.")
(defvar-local mail-util--meta nil
  "Alist of run metadata (inbox, counts).")
(defvar-local mail-util--marks nil
  "Hash table mapping cluster index to `approved' or `rejected'.")
(defvar-local mail-util--expanded nil
  "Hash table mapping cluster index to non-nil when details are shown.")

(defconst mail-util--review-buffer "*mail-util-review*")

;;;; Destination helpers

(defun mail-util--destination (cluster)
  "Return (DOTPATH . EXISTINGP) for CLUSTER's destination."
  (let ((dest (alist-get 'destination cluster)))
    (cond
     ((alist-get 'existing dest)
      (cons (alist-get 'dotpath (alist-get 'existing dest)) t))
     ((alist-get 'new dest)
      (cons (alist-get 'dotpath (alist-get 'new dest)) nil))
     (t (cons "?" nil)))))

;;;; Running the CLI

(defun mail-util--suggest-args ()
  "Build the argument list for `mail-util suggest'."
  (append (list "suggest"
                "--inbox" mail-util-inbox
                "--min-count" (number-to-string mail-util-min-count))
          (when mail-util-root (list "--root" (expand-file-name mail-util-root)))))

(defun mail-util--run-suggest (callback)
  "Run `mail-util suggest' asynchronously and call CALLBACK with parsed JSON.
CALLBACK is invoked in the review buffer on success; on failure an
error is signaled with the CLI's stderr."
  (let* ((stdout (generate-new-buffer " *mail-util-stdout*"))
         (stderr (generate-new-buffer " *mail-util-stderr*"))
         (args (mail-util--suggest-args)))
    (message "mail-util: analyzing inbox (%s) …" mail-util-inbox)
    (make-process
     :name "mail-util-suggest"
     :buffer stdout
     :stderr stderr
     :noquery t
     :command (cons (or (executable-find mail-util-executable) mail-util-executable)
                    args)
     :sentinel
     (lambda (proc event)
       (when (memq (process-status proc) '(exit signal))
         (unwind-protect
             (if (and (eq (process-status proc) 'exit)
                      (zerop (process-exit-status proc)))
                 (let ((data (with-current-buffer stdout
                               (goto-char (point-min))
                               (json-parse-buffer :object-type 'alist
                                                  :array-type 'list
                                                  :null-object nil
                                                  :false-object nil))))
                   (funcall callback data))
               (let ((err (string-trim (with-current-buffer stderr (buffer-string)))))
                 (message "mail-util failed (%s): %s"
                          (string-trim event)
                          (if (string-empty-p err) "see *Messages*" err))))
           (kill-buffer stdout)
           (kill-buffer stderr)))))))

;;;; Rendering

(defun mail-util--mark (index)
  "Return the mark symbol for cluster INDEX, or nil."
  (gethash index mail-util--marks))

(defun mail-util--marker-string (index)
  "Return the two-char marker for cluster INDEX."
  (pcase (mail-util--mark index)
    ('approved (propertize "A" 'face 'mail-util-approved))
    ('rejected (propertize "R" 'face 'mail-util-rejected))
    (_ " ")))

(defun mail-util--insert-cluster (index cluster)
  "Insert the line(s) for CLUSTER at INDEX."
  (let* ((dest (mail-util--destination cluster))
         (dotpath (car dest))
         (existingp (cdr dest))
         (rejected (eq (mail-util--mark index) 'rejected))
         (start (point)))
    (insert
     (format "  [%s]  %s  %-14s %-6s → %s%s\n"
             (mail-util--marker-string index)
             (propertize (format "%5d" (alist-get 'count cluster)) 'face 'mail-util-count)
             (alist-get 'signal cluster)
             (alist-get 'confidence cluster)
             (propertize dotpath 'face 'mail-util-destination)
             (concat (if existingp
                         (propertize "  (existing)" 'face 'mail-util-approved)
                       "")
                     "   "
                     (propertize (format "(%s)" (alist-get 'key cluster))
                                 'face 'mail-util-key))))
    ;; Tag the whole main line with the cluster index for navigation.
    (put-text-property start (point) 'mail-util-cluster index)
    (when rejected
      (put-text-property start (point) 'face 'mail-util-rejected))
    ;; Optional detail block.
    (when (gethash index mail-util--expanded)
      (let ((senders (alist-get 'sample_senders cluster))
            (subjects (alist-get 'sample_subjects cluster)))
        (dolist (s senders)
          (insert (format "          from: %s\n" (propertize s 'face 'mail-util-key))))
        (dolist (s subjects)
          (insert (format "          subj: %s\n" (propertize s 'face 'shadow))))))))

(defun mail-util--render ()
  "Render the review buffer from current state, preserving the current cluster."
  (let ((keep (mail-util--index-at-point))
        (inhibit-read-only t))
    (erase-buffer)
    (let* ((n (length mail-util--clusters))
           (approved (cl-count 'approved (hash-table-values mail-util--marks)))
           (rejected (cl-count 'rejected (hash-table-values mail-util--marks))))
      (insert (propertize
               (format "mail-util review — inbox %s: %s of %s messages in %d clusters\n"
                       (alist-get 'inbox mail-util--meta)
                       (alist-get 'clustered_messages mail-util--meta)
                       (alist-get 'inbox_messages mail-util--meta)
                       n)
               'face 'bold))
      (insert (propertize
               (format "approved: %d   rejected: %d   pending: %d\n"
                       approved rejected (- n approved rejected))
               'face 'shadow))
      (insert (propertize
               "keys: n/p move · a approve · r reject · u unset · TAB details · g refresh · x export · q quit\n\n"
               'face 'shadow)))
    (dotimes (i (length mail-util--clusters))
      (mail-util--insert-cluster i (aref mail-util--clusters i)))
    (goto-char (point-min))
    (when keep (mail-util--goto-index keep))))

;;;; Navigation & point<->index

(defun mail-util--index-at-point ()
  "Return the cluster index at point, or nil."
  (get-text-property (line-beginning-position) 'mail-util-cluster))

(defun mail-util--goto-index (index)
  "Move point to the main line of cluster INDEX, if present."
  (goto-char (point-min))
  (let ((match (text-property-search-forward 'mail-util-cluster index #'eq)))
    (when match
      (goto-char (prop-match-beginning match))
      (beginning-of-line))))

(defun mail-util-next ()
  "Move to the next cluster's main line."
  (interactive)
  (let ((cur (mail-util--index-at-point))
        (found nil))
    (save-excursion
      (forward-line 1)
      (while (and (not (eobp)) (not found))
        (let ((idx (get-text-property (line-beginning-position) 'mail-util-cluster)))
          (if (and idx (not (equal idx cur)))
              (setq found (line-beginning-position))
            (forward-line 1)))))
    (if found (goto-char found) (message "No more clusters"))))

(defun mail-util-previous ()
  "Move to the previous cluster's main line."
  (interactive)
  (let ((cur (mail-util--index-at-point))
        (found nil))
    (save-excursion
      (while (and (not (bobp)) (not found))
        (forward-line -1)
        (let ((idx (get-text-property (line-beginning-position) 'mail-util-cluster)))
          (when (and idx (not (equal idx cur)))
            (setq found (line-beginning-position))))))
    (if found (goto-char found) (message "No previous cluster"))))

;;;; Commands

(defun mail-util--set-mark (mark)
  "Set MARK on the cluster at point and advance."
  (let ((index (mail-util--index-at-point)))
    (if (null index)
        (message "Point is not on a cluster")
      (if mark
          (puthash index mark mail-util--marks)
        (remhash index mail-util--marks))
      (mail-util--render)
      (mail-util--goto-index index)
      (mail-util-next))))

(defun mail-util-approve ()
  "Approve the cluster at point."
  (interactive)
  (mail-util--set-mark 'approved))

(defun mail-util-reject ()
  "Reject the cluster at point."
  (interactive)
  (mail-util--set-mark 'rejected))

(defun mail-util-unset ()
  "Clear the mark on the cluster at point."
  (interactive)
  (mail-util--set-mark nil))

(defun mail-util-toggle-details ()
  "Toggle the sample senders/subjects for the cluster at point."
  (interactive)
  (let ((index (mail-util--index-at-point)))
    (when index
      (if (gethash index mail-util--expanded)
          (remhash index mail-util--expanded)
        (puthash index t mail-util--expanded))
      (mail-util--render)
      (mail-util--goto-index index))))

(defun mail-util-approve-all-high ()
  "Approve every cluster whose confidence is \"high\"."
  (interactive)
  (dotimes (i (length mail-util--clusters))
    (when (equal (alist-get 'confidence (aref mail-util--clusters i)) "high")
      (puthash i 'approved mail-util--marks)))
  (mail-util--render)
  (message "Approved all high-confidence clusters"))

(defun mail-util-refresh ()
  "Re-run `mail-util suggest' and redraw, keeping existing marks by cluster key."
  (interactive)
  (let ((prev-marks (mail-util--marks-by-key)))
    (mail-util--run-suggest
     (lambda (data)
       (with-current-buffer (get-buffer-create mail-util--review-buffer)
         (mail-util--load data)
         (mail-util--restore-marks-by-key prev-marks)
         (mail-util--render)
         (message "mail-util: %d clusters" (length mail-util--clusters)))))))

(defun mail-util--marks-by-key ()
  "Return an alist of cluster-key -> mark for currently marked clusters."
  (let (acc)
    (when mail-util--clusters
      (maphash (lambda (i mark)
                 (push (cons (alist-get 'key (aref mail-util--clusters i)) mark) acc))
               mail-util--marks))
    acc))

(defun mail-util--restore-marks-by-key (key-marks)
  "Re-apply KEY-MARKS (key -> mark alist) onto the freshly loaded clusters."
  (dotimes (i (length mail-util--clusters))
    (let ((mark (alist-get (alist-get 'key (aref mail-util--clusters i))
                           key-marks nil nil #'equal)))
      (when mark (puthash i mark mail-util--marks)))))

(defun mail-util-export-approved (file)
  "Write the approved clusters to FILE as JSON.
This is a precursor to the CLI's forthcoming `plan'/`apply': it records
the destination folders and message UIDs you approved."
  (interactive (list (read-file-name "Export approved to: " nil mail-util-export-file)))
  ;; Capture buffer-local state here — `with-temp-file' switches buffers, where
  ;; `mail-util--meta' would be its (nil) default.
  (let ((inbox (alist-get 'inbox mail-util--meta))
        (root (alist-get 'root mail-util--meta))
        (approved
         (cl-loop for i from 0 below (length mail-util--clusters)
                  when (eq (mail-util--mark i) 'approved)
                  collect (let* ((c (aref mail-util--clusters i))
                                 (dest (mail-util--destination c)))
                            (list :key (alist-get 'key c)
                                  :signal (alist-get 'signal c)
                                  :destination (car dest)
                                  :existing (if (cdr dest) t :false)
                                  :count (alist-get 'count c)
                                  :uids (apply #'vector (alist-get 'uids c)))))))
    (if (null approved)
        (message "No approved clusters to export")
      (with-temp-file file
        (insert (json-serialize
                 (list :inbox inbox
                       :root root
                       :approved (apply #'vector approved)))))
      (message "Wrote %d approved cluster(s) to %s" (length approved) file))))

;;;; Major mode & entry point

(defvar-keymap mail-util-review-mode-map
  :doc "Keymap for `mail-util-review-mode'."
  "n" #'mail-util-next
  "p" #'mail-util-previous
  "a" #'mail-util-approve
  "r" #'mail-util-reject
  "u" #'mail-util-unset
  "A" #'mail-util-approve-all-high
  "TAB" #'mail-util-toggle-details
  "g" #'mail-util-refresh
  "x" #'mail-util-export-approved
  "q" #'quit-window)

(define-derived-mode mail-util-review-mode special-mode "mail-util"
  "Major mode for reviewing mail-util sorting suggestions."
  (setq truncate-lines t)
  (setq mail-util--marks (make-hash-table :test 'eql))
  (setq mail-util--expanded (make-hash-table :test 'eql)))

(defun mail-util--load (data)
  "Populate buffer-local state from parsed JSON DATA."
  (setq mail-util--clusters (apply #'vector (alist-get 'clusters data)))
  (setq mail-util--meta data)
  (unless (hash-table-p mail-util--marks)
    (setq mail-util--marks (make-hash-table :test 'eql)))
  (unless (hash-table-p mail-util--expanded)
    (setq mail-util--expanded (make-hash-table :test 'eql))))

;;;###autoload
(defun mail-util-review ()
  "Run `mail-util suggest' and open the review buffer."
  (interactive)
  (mail-util--run-suggest
   (lambda (data)
     (with-current-buffer (get-buffer-create mail-util--review-buffer)
       (unless (derived-mode-p 'mail-util-review-mode)
         (mail-util-review-mode))
       (mail-util--load data)
       (mail-util--render)
       (pop-to-buffer (current-buffer))
       (message "mail-util: %d clusters (%s of %s messages)"
                (length mail-util--clusters)
                (alist-get 'clustered_messages mail-util--meta)
                (alist-get 'inbox_messages mail-util--meta))))))

(provide 'mail-util)
;;; mail-util.el ends here
