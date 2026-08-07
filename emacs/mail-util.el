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

(defcustom mail-util-imap-host nil
  "IMAP server hostname for `mail-util-probe' and a real apply.
Credentials are read from ~/.netrc for this machine (as mbsync does)."
  :type '(choice (const :tag "Unset" nil) string))

(defcustom mail-util-imap-user nil
  "IMAP login for `mail-util-probe' and a real apply.
Needed to pick the right account when several share `mail-util-imap-host';
the matching ~/.netrc entry supplies the password. Leave nil to use the
first netrc entry for the host."
  :type '(choice (const :tag "First netrc entry" nil) string))

(defcustom mail-util-imap-port 993
  "IMAP server port."
  :type 'natnum)

(defcustom mail-util-mbsync-channel nil
  "If set, a real apply runs `mbsync <channel>' afterward to reconcile the cache."
  :type '(choice (const :tag "Don't reconcile" nil) string))

(defcustom mail-util-mover "imap"
  "Which mover a built plan targets.
\"imap\" moves messages server-side (default, safest); \"local\" manipulates
the Maildir offline. A real apply reads this from the plan."
  :type '(choice (const "imap") (const "local")))

(defcustom mail-util-sieve-port 4190
  "ManageSieve port for deploying Sieve rules."
  :type 'natnum)

(defcustom mail-util-sieve-script nil
  "Target Sieve script name to deploy into.
Nil deploys into the account's active script (or \"mail-util\" if none)."
  :type '(choice (const :tag "Active script" nil) string))

(defcustom mail-util-sieve-auto-merge t
  "When non-nil, building a plan fetches the server's Sieve script and shows the
merged result automatically (when `mail-util-imap-host' is set), so the plan
always reflects what would actually be deployed. Set nil to fetch only on `e'."
  :type 'boolean)

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

(defvar mail-util--last-json nil
  "Raw JSON text from the most recent successful CLI run, for saving to a file.")

(defun mail-util--common-args ()
  "The `--inbox'/`--min-count' args shared by suggest and plan."
  (list "--inbox" mail-util-inbox
        "--min-count" (number-to-string mail-util-min-count)))

(defun mail-util--run-json (subargs callback)
  "Run `mail-util' with SUBARGS (subcommand + flags) asynchronously.
On success, set `mail-util--last-json' to the raw stdout and call
CALLBACK with the parsed JSON (alists / lists).  The account root is
prepended from `mail-util-root' when set."
  (let* ((stdout (generate-new-buffer " *mail-util-stdout*"))
         (stderr (generate-new-buffer " *mail-util-stderr*"))
         (root-args (when mail-util-root
                      (list "--root" (expand-file-name mail-util-root))))
         (command (cons (or (executable-find mail-util-executable) mail-util-executable)
                        (append root-args subargs))))
    (make-process
     :name "mail-util"
     :buffer stdout
     :stderr stderr
     :noquery t
     :command command
     :sentinel
     (lambda (proc event)
       (when (memq (process-status proc) '(exit signal))
         (unwind-protect
             (if (and (eq (process-status proc) 'exit)
                      (zerop (process-exit-status proc)))
                 (let* ((raw (with-current-buffer stdout (buffer-string)))
                        (data (with-temp-buffer
                                (insert raw)
                                (goto-char (point-min))
                                (json-parse-buffer :object-type 'alist
                                                   :array-type 'list
                                                   :null-object nil
                                                   :false-object nil))))
                   (setq mail-util--last-json raw)
                   (funcall callback data))
               (let ((err (string-trim (with-current-buffer stderr (buffer-string)))))
                 (message "mail-util failed (%s): %s"
                          (string-trim event)
                          (if (string-empty-p err) "see *Messages*" err))))
           (kill-buffer stdout)
           (kill-buffer stderr)))))))

(defun mail-util--run-suggest (callback)
  "Run `mail-util suggest' and call CALLBACK with parsed JSON."
  (message "mail-util: analyzing inbox (%s) …" mail-util-inbox)
  (mail-util--run-json (cons "suggest" (mail-util--common-args)) callback))

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
               "keys: n/p move · a/r/u approve/reject/unset · A all-high · TAB details · P plan · g refresh · x export · q quit\n\n"
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

;;;; Plan / verify

(defvar-local mail-util--plan nil
  "Parsed plan alist shown in the current `*mail-util-plan*' buffer.")
(defvar-local mail-util--plan-json nil
  "Raw plan JSON text backing the current plan buffer (for verify/save).")
(defvar-local mail-util--plan-merged-sieve nil
  "Merged Sieve script fetched from the server, or nil if not previewed yet.")
(defvar-local mail-util--plan-existing-sieve nil
  "The server's current Sieve script, fetched during a preview.")
(defvar-local mail-util--plan-merged-script nil
  "Name of the server script the merge targets.")

(defconst mail-util--plan-buffer "*mail-util-plan*")

(defun mail-util--approved-keys-file ()
  "Write the approved cluster keys to a temp file for `plan --approved'.
Return the file path, or nil when no clusters are approved (plan everything)."
  (let ((approved
         (cl-loop for i from 0 below (length mail-util--clusters)
                  when (eq (mail-util--mark i) 'approved)
                  collect (list :key (alist-get 'key (aref mail-util--clusters i))))))
    (when approved
      (let ((file (make-temp-file "mail-util-approved" nil ".json")))
        (with-temp-file file
          (insert (json-serialize (list :approved (apply #'vector approved)))))
        file))))

(defun mail-util-build-plan (&optional choose-mover)
  "Build a sorting plan and show it in `*mail-util-plan*'.
Uses the approved clusters if any are marked, otherwise every surfaced
cluster.  Run from the review buffer.

The plan targets `mail-util-mover'.  With a prefix argument, prompt for the
mover instead, so you can pick imap/local without touching the variable."
  (interactive "P")
  (let* ((mover (if choose-mover
                    (completing-read "Mover: " '("imap" "local") nil t nil nil
                                     (format "%s" mail-util-mover))
                  ;; Coerce so a symbol value (e.g. `local) also works.
                  (format "%s" mail-util-mover)))
         (approved-file (and mail-util--clusters (mail-util--approved-keys-file)))
         (args (append (list "plan") (mail-util--common-args)
                       (list "--mover" mover)
                       (when approved-file (list "--approved" approved-file)))))
    (message "mail-util: building plan (%s mover) …" mover)
    (mail-util--run-json
     args
     (lambda (plan)
       (when approved-file (ignore-errors (delete-file approved-file)))
       (mail-util--show-plan plan mail-util--last-json)))))

(defun mail-util--show-plan (plan raw)
  "Store PLAN (parsed) with RAW json text and render the plan buffer."
  (with-current-buffer (get-buffer-create mail-util--plan-buffer)
    (unless (derived-mode-p 'mail-util-plan-mode)
      (mail-util-plan-mode))
    (setq mail-util--plan plan
          mail-util--plan-json raw
          mail-util--plan-merged-sieve nil
          mail-util--plan-existing-sieve nil
          mail-util--plan-merged-script nil)
    (mail-util--render-plan)
    (pop-to-buffer (current-buffer))
    ;; By default, immediately fetch the server's Sieve and show the merged result, so
    ;; the plan reflects what would actually be deployed (never just the new rules).
    (when (and mail-util-sieve-auto-merge mail-util-imap-host)
      (mail-util--fetch-merged-sieve (current-buffer) nil))))

(defun mail-util--render-plan ()
  "Render the current plan buffer. Shows the server-merged Sieve if it has been
fetched (via `mail-util-preview-sieve'), otherwise the generated block."
  (let* ((plan mail-util--plan)
         (pc (alist-get 'precheck plan))
         (sep (concat (propertize (make-string 64 ?─) 'face 'shadow) "\n"))
         (inhibit-read-only t))
    (erase-buffer)
    (insert (propertize (format "mail-util plan %s\n" (alist-get 'plan_id plan)) 'face 'bold))
    (insert (format "mover: %s   separator: %s   inbox: %s\n"
                    (alist-get 'mover plan) (alist-get 'separator plan)
                    (alist-get 'inbox plan)))
    (insert (format "actions: %s   folders to create: %s   unresolved: %s   missing msg-id: %s\n\n"
                    (alist-get 'actions pc)
                    (length (alist-get 'folders_to_create plan))
                    (alist-get 'actions_unresolved pc)
                    (alist-get 'actions_missing_message_id pc)))
    (insert (propertize "Folders to create\n" 'face 'bold))
    (if (alist-get 'folders_to_create plan)
        (dolist (f (alist-get 'folders_to_create plan))
          (insert (format "  %s  →  %s\n"
                          (propertize (alist-get 'dotpath f) 'face 'mail-util-destination)
                          (alist-get 'imap_name f))))
      (insert "  (none — all destinations already exist)\n"))
    (if mail-util--plan-merged-sieve
        (progn
          (insert (propertize
                   (format "\nSieve — MERGED with server script %S (what D would deploy)\n"
                           (or mail-util--plan-merged-script "?"))
                   'face 'bold))
          (insert sep)
          (insert mail-util--plan-merged-sieve)
          (insert sep)
          (insert (propertize "your hand-written rules are preserved; only the mail-util block changes · E views the server script\n"
                              'face 'shadow)))
      (insert (propertize "\nSieve script (generated block — not yet merged with the server)\n" 'face 'bold))
      (insert sep)
      (insert (or (alist-get 'sieve_text plan) ""))
      (insert sep)
      (insert (propertize "press e to fetch your server script and preview the merged result\n"
                          'face 'shadow)))
    (insert (propertize "\nkeys: v verify · d dry-run · X apply · e preview-merged-sieve · D deploy sieve · w write · s save · q quit\n"
                        'face 'shadow)))
  (goto-char (point-min)))

(defun mail-util-verify ()
  "Verify the plan in the current plan buffer against the cache."
  (interactive)
  (unless mail-util--plan-json (user-error "No plan in this buffer"))
  (let ((file (make-temp-file "mail-util-plan" nil ".json"))
        (json mail-util--plan-json))
    (with-temp-file file (insert json))
    (message "mail-util: verifying …")
    (mail-util--run-json
     (list "verify" "--plan" file)
     (lambda (res)
       (ignore-errors (delete-file file))
       (message "verify: %s  resolved %s/%s  unresolved %s  into-excluded %s"
                (if (alist-get 'ok res) "OK" "PROBLEM")
                (alist-get 'resolved res) (alist-get 'actions res)
                (alist-get 'unresolved res) (alist-get 'actions_into_excluded res))))))

(defun mail-util-write-sieve (file)
  "Write the current plan's sieve script to FILE."
  (interactive (list (read-file-name "Write sieve to: ")))
  (unless mail-util--plan (user-error "No plan in this buffer"))
  (let ((text (alist-get 'sieve_text mail-util--plan)))
    (with-temp-file file (insert text))
    (message "Wrote sieve to %s" file)))

(defun mail-util-save-plan (file)
  "Save the current plan's raw JSON to FILE."
  (interactive (list (read-file-name "Save plan JSON to: ")))
  (unless mail-util--plan-json (user-error "No plan in this buffer"))
  (let ((json mail-util--plan-json))
    (with-temp-file file (insert json))
    (message "Wrote plan to %s" file)))

(defun mail-util--sieve-args (deploy)
  "Build `mail-util sieve' args for the current plan; DEPLOY adds --deploy."
  (append (list "--imap-host" mail-util-imap-host
                "--sieve-port" (number-to-string mail-util-sieve-port))
          (when mail-util-imap-user (list "--imap-user" mail-util-imap-user))
          (when mail-util-sieve-script (list "--script-name" mail-util-sieve-script))
          (when deploy (list "--deploy"))))

(defun mail-util--fetch-merged-sieve (planbuf show-compare)
  "Fetch the server Sieve for the plan in PLANBUF and update its merged view.
Read-only (uploads nothing). If SHOW-COMPARE is non-nil, also pop
`*mail-util-sieve*' with the existing vs. merged scripts side by side. Fails
gracefully — on any error the plan keeps showing the generated block."
  (when (and (buffer-live-p planbuf) mail-util-imap-host)
    (let* ((json (buffer-local-value 'mail-util--plan-json planbuf))
           (file (make-temp-file "mail-util-plan" nil ".json"))
           (args (append (list "sieve" "--plan" file) (mail-util--sieve-args nil))))
      (with-temp-file file (insert json))
      (message "mail-util: fetching server Sieve from %s …" mail-util-imap-host)
      (mail-util--run-json
       args
       (lambda (res)
         (ignore-errors (delete-file file))
         (let ((existing (alist-get 'existing res))
               (merged (alist-get 'merged res))
               (script (alist-get 'script res)))
           (when (buffer-live-p planbuf)
             (with-current-buffer planbuf
               (setq mail-util--plan-existing-sieve existing
                     mail-util--plan-merged-sieve merged
                     mail-util--plan-merged-script script)
               (let ((inhibit-read-only t)) (mail-util--render-plan))))
           (when show-compare
             (with-current-buffer (get-buffer-create "*mail-util-sieve*")
               (let ((inhibit-read-only t))
                 (erase-buffer)
                 (insert (format "═══ Existing server script: %s ═══\n\n" script))
                 (insert (if (and existing (> (length existing) 0)) existing "(no script on the server yet)\n"))
                 (insert "\n\n═══ Merged — what D would deploy ═══\n\n")
                 (insert (or merged ""))
                 (goto-char (point-min)))
               (when (fboundp 'sieve-mode) (ignore-errors (sieve-mode)))
               (view-mode 1))
             (display-buffer "*mail-util-sieve*"))
           (message "Server Sieve merged into the plan (script %s)" script)))))))

(defun mail-util-preview-sieve ()
  "Fetch the server's Sieve script and preview the merged result (read-only).
Updates this plan buffer's Sieve section and opens `*mail-util-sieve*' showing
your current server script vs. the merged result. Uploads nothing."
  (interactive)
  (unless mail-util--plan-json (user-error "No plan in this buffer"))
  (unless mail-util-imap-host (user-error "Set `mail-util-imap-host' first"))
  (mail-util--fetch-merged-sieve (current-buffer) t))

(defun mail-util-view-existing-sieve ()
  "Show the server's current Sieve script (fetched by a prior preview)."
  (interactive)
  (unless mail-util--plan-existing-sieve
    (user-error "Run `mail-util-preview-sieve' (e) first to fetch the server script"))
  (with-current-buffer (get-buffer-create "*mail-util-sieve*")
    (let ((inhibit-read-only t))
      (erase-buffer)
      (insert (format "═══ Server script: %s ═══\n\n" (or mail-util--plan-merged-script "?")))
      (insert (if (> (length mail-util--plan-existing-sieve) 0)
                  mail-util--plan-existing-sieve
                "(no script on the server yet)\n"))
      (goto-char (point-min)))
    (when (fboundp 'sieve-mode) (ignore-errors (sieve-mode)))
    (view-mode 1)
    (pop-to-buffer (current-buffer))))

(defun mail-util-deploy-sieve ()
  "Deploy the current plan's Sieve rules to the server (upload + activate).
Prompts for confirmation. Preview first with `mail-util-preview-sieve' (e)."
  (interactive)
  (unless mail-util--plan-json (user-error "No plan in this buffer"))
  (unless mail-util-imap-host (user-error "Set `mail-util-imap-host' first"))
  (unless (yes-or-no-p
           (format "Deploy Sieve rules to %s? (changes server-side filtering) "
                   mail-util-imap-host))
    (user-error "Aborted"))
  (let* ((file (make-temp-file "mail-util-plan" nil ".json"))
         (json mail-util--plan-json)
         (args (append (list "sieve" "--plan" file) (mail-util--sieve-args t))))
    (with-temp-file file (insert json))
    (message "mail-util: deploying Sieve on %s …" mail-util-imap-host)
    (mail-util--run-json
     args
     (lambda (res)
       (ignore-errors (delete-file file))
       (message "Sieve deployed to script %S (%s bytes, %s)"
                (alist-get 'script res) (alist-get 'bytes res)
                (if (alist-get 'created res) "created" "updated"))))))

;;;; Apply (dry-run) with live NDJSON progress

(defvar-local mail-util--apply-counts nil
  "Hash of running counters for the current apply buffer.")
(defvar-local mail-util--apply-log nil
  "List of notable apply events, newest first.")
(defvar-local mail-util--apply-title nil
  "Title line for the current apply buffer.")
(defvar-local mail-util--apply-real nil
  "Non-nil when the current apply buffer is a real (mutating) run.")

(defconst mail-util--apply-buffer "*mail-util-apply*")

(defun mail-util--run-stream (subargs on-line on-done)
  "Run `mail-util' with SUBARGS, calling ON-LINE with each parsed JSON stdout
line and ON-DONE with (EXIT-STATUS STDERR-BUFFER) when the process finishes."
  (let* ((root-args (when mail-util-root
                      (list "--root" (expand-file-name mail-util-root))))
         (command (cons (or (executable-find mail-util-executable) mail-util-executable)
                        (append root-args subargs)))
         (stderr (generate-new-buffer " *mail-util-apply-stderr*"))
         (acc ""))
    (cl-flet ((parse (s) (json-parse-string s :object-type 'alist :array-type 'list
                                            :null-object nil :false-object nil)))
      (make-process
       :name "mail-util-apply"
       :buffer nil
       :stderr stderr
       :noquery t
       :command command
       :filter
       (lambda (_proc chunk)
         (setq acc (concat acc chunk))
         (let ((lines (split-string acc "\n")))
           (setq acc (car (last lines)))
           (dolist (line (butlast lines))
             (unless (string-empty-p line)
               (funcall on-line (parse line))))))
       :sentinel
       (lambda (proc _event)
         (when (memq (process-status proc) '(exit signal))
           (unless (string-empty-p (string-trim acc))
             (funcall on-line (parse acc)))
           (funcall on-done (process-exit-status proc) stderr)))))))

(defun mail-util--inc (key &optional n)
  "Add N (default 1) to counter KEY in the apply buffer."
  (puthash key (+ (gethash key mail-util--apply-counts 0) (or n 1))
           mail-util--apply-counts))

(defun mail-util--apply-render ()
  "Redraw the apply buffer from its counters and log."
  (let ((inhibit-read-only t)
        (c mail-util--apply-counts))
    (erase-buffer)
    (insert (propertize (format "mail-util apply (%s) — %s\n"
                                (if mail-util--apply-real "REAL" "dry-run")
                                (or mail-util--apply-title "…"))
                        'face (if mail-util--apply-real 'error 'bold)))
    (insert (format "folders %d · simulated %d · moved %d · skipped %d · failed %d\n\n"
                    (gethash 'folders c 0) (gethash 'simulated c 0)
                    (gethash 'moved c 0) (gethash 'skipped c 0) (gethash 'failed c 0)))
    (dolist (line (reverse (seq-take mail-util--apply-log 25)))
      (insert "  " line "\n"))
    (goto-char (point-max))))

(defun mail-util--apply-record (rec)
  "Fold one parsed journal REC into the apply buffer, rendering when notable."
  (let ((event (alist-get 'event rec))
        (render t))
    (pcase event
      ("plan_loaded"
       (setq mail-util--apply-title
             (format "%s · %s actions" (alist-get 'plan_id rec) (alist-get 'actions rec))))
      ("folder_ensured"
       (mail-util--inc 'folders)
       (push (format "＋ folder %s" (alist-get 'dotpath rec)) mail-util--apply-log))
      ("action_begin" (setq render nil))
      ("action_moved"
       (mail-util--inc (if (equal (alist-get 'outcome rec) "moved") 'moved 'simulated))
       ;; Throttle: only redraw every 100th action to stay snappy on big plans.
       (setq render (zerop (% (+ (gethash 'moved mail-util--apply-counts 0)
                                 (gethash 'simulated mail-util--apply-counts 0))
                              100))))
      ("action_skipped" (mail-util--inc 'skipped) (setq render nil))
      ("action_failed"
       (mail-util--inc 'failed)
       (push (format "✗ action %s: %s" (alist-get 'index rec) (alist-get 'error rec))
             mail-util--apply-log))
      ("fatal" (push (format "FATAL: %s" (alist-get 'error rec)) mail-util--apply-log))
      ("reconciled" (push "reconciled (mbsync)" mail-util--apply-log))
      ("done"
       (push (format "DONE — moved %s · simulated %s · skipped %s · failed %s"
                     (alist-get 'moved rec) (alist-get 'simulated rec)
                     (alist-get 'skipped rec) (alist-get 'failed rec))
             mail-util--apply-log)))
    (when render (mail-util--apply-render))))

(defun mail-util--start-apply (real)
  "Run apply on the current plan, streaming progress into `*mail-util-apply*'.
When REAL is non-nil this MOVES MAIL (after confirmation); otherwise it is a
dry run that mutates nothing."
  (unless mail-util--plan-json (user-error "No plan in this buffer"))
  (let ((mover (or (alist-get 'mover mail-util--plan) "imap")))
    (when real
      (when (equal mover "imap")
        (unless mail-util-imap-host
          (user-error "Set `mail-util-imap-host' for a real IMAP apply")))
      (let ((n (length (alist-get 'actions mail-util--plan))))
        (unless (yes-or-no-p
                 (format "REALLY move %d message(s) [%s mover]? " n mover))
          (user-error "Aborted"))))
    (let* ((file (make-temp-file "mail-util-plan" nil ".json"))
           (json mail-util--plan-json)
           (buf (get-buffer-create mail-util--apply-buffer))
           (args (append (list "apply" "--plan" file (if real "--yes" "--dry-run"))
                         (when (and real (equal mover "imap"))
                           (append (list "--imap-host" mail-util-imap-host
                                         "--imap-port" (number-to-string mail-util-imap-port))
                                   (when mail-util-imap-user
                                     (list "--imap-user" mail-util-imap-user))))
                         (when (and real mail-util-mbsync-channel)
                           (list "--mbsync-channel" mail-util-mbsync-channel)))))
    (with-temp-file file (insert json))
    (with-current-buffer buf
      (mail-util-apply-mode)
      (setq mail-util--apply-counts (make-hash-table :test 'eq)
            mail-util--apply-log nil
            mail-util--apply-title nil
            mail-util--apply-real real)
      (let ((inhibit-read-only t))
        (erase-buffer)
        (insert (if real "starting REAL apply…\n" "starting dry-run…\n"))))
    (pop-to-buffer buf)
    (mail-util--run-stream
     args
     (lambda (rec) (when (buffer-live-p buf)
                     (with-current-buffer buf (mail-util--apply-record rec))))
     (lambda (status stderr-buf)
       (ignore-errors (delete-file file))
       (when (buffer-live-p buf)
         (with-current-buffer buf
           (when (/= status 0)
             (push (concat "stderr: " (string-trim
                                       (with-current-buffer stderr-buf (buffer-string))))
                   mail-util--apply-log))
           (mail-util--apply-render)))
       (kill-buffer stderr-buf)
       (message "mail-util apply (%s) finished (status %s)"
                (if real "REAL" "dry-run") status))))))

(defun mail-util-apply-dry-run ()
  "Dry-run the current plan (moves nothing)."
  (interactive)
  (mail-util--start-apply nil))

(defun mail-util-apply-real ()
  "Execute the current plan for real: move mail on the server. Prompts first."
  (interactive)
  (mail-util--start-apply t))

(defun mail-util-probe ()
  "Probe the IMAP server for its hierarchy separator and MOVE capability."
  (interactive)
  (unless mail-util-imap-host (user-error "Set `mail-util-imap-host' first"))
  (message "mail-util: probing %s …" mail-util-imap-host)
  (mail-util--run-json
   (append (list "probe" "--imap-host" mail-util-imap-host
                 "--imap-port" (number-to-string mail-util-imap-port))
           (when mail-util-imap-user (list "--imap-user" mail-util-imap-user)))
   (lambda (res)
     (message "probe %s: separator %S · server MOVE: %s"
              (alist-get 'host res) (alist-get 'separator res)
              (if (alist-get 'has_move res) "yes" "no")))))

;;;; Major mode & entry point

(defvar mail-util-review-mode-map (make-sparse-keymap)
  "Keymap for `mail-util-review-mode'.")

(defvar mail-util-plan-mode-map (make-sparse-keymap)
  "Keymap for `mail-util-plan-mode'.")

(defvar mail-util-apply-mode-map (make-sparse-keymap)
  "Keymap for `mail-util-apply-mode'.")

;; Bind keys imperatively (not via `defvar-keymap', which — like `defvar' — only
;; assigns when unbound) so re-loading this file updates the bindings on the existing
;; keymap objects, and therefore in any already-open review/plan buffers.
(pcase-dolist (`(,key . ,cmd)
               '(("n" . mail-util-next)
                 ("p" . mail-util-previous)
                 ("a" . mail-util-approve)
                 ("r" . mail-util-reject)
                 ("u" . mail-util-unset)
                 ("A" . mail-util-approve-all-high)
                 ("TAB" . mail-util-toggle-details)
                 ("P" . mail-util-build-plan)
                 ("g" . mail-util-refresh)
                 ("x" . mail-util-export-approved)
                 ("q" . quit-window)))
  (keymap-set mail-util-review-mode-map key cmd))

(pcase-dolist (`(,key . ,cmd)
               '(("v" . mail-util-verify)
                 ("d" . mail-util-apply-dry-run)
                 ("X" . mail-util-apply-real)
                 ("e" . mail-util-preview-sieve)
                 ("E" . mail-util-view-existing-sieve)
                 ("w" . mail-util-write-sieve)
                 ("D" . mail-util-deploy-sieve)
                 ("s" . mail-util-save-plan)
                 ("q" . quit-window)))
  (keymap-set mail-util-plan-mode-map key cmd))

(keymap-set mail-util-apply-mode-map "q" #'quit-window)

(define-derived-mode mail-util-plan-mode special-mode "mail-util-plan"
  "Major mode for viewing a mail-util sorting plan."
  (setq truncate-lines nil))

(define-derived-mode mail-util-apply-mode special-mode "mail-util-apply"
  "Major mode for the live apply (dry-run) progress buffer."
  (setq truncate-lines t))

(define-derived-mode mail-util-review-mode special-mode "mail-util"
  "Major mode for reviewing mail-util sorting suggestions."
  (setq truncate-lines t)
  (setq mail-util--marks (make-hash-table :test 'eql))
  (setq mail-util--expanded (make-hash-table :test 'eql)))

(defun mail-util--load (data)
  "Populate buffer-local state from parsed JSON DATA.
Marks and expansion are keyed by cluster index, which is only meaningful for
the freshly loaded list — so reset them here. `mail-util-refresh' captures the
marks by cluster key before reloading and re-applies them afterward, so a
refresh preserves your selections without letting stale index marks leak onto
whatever cluster now sits at that index."
  (setq mail-util--clusters (apply #'vector (alist-get 'clusters data)))
  (setq mail-util--meta data)
  (setq mail-util--marks (make-hash-table :test 'eql))
  (setq mail-util--expanded (make-hash-table :test 'eql)))

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
