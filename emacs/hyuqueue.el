;;; hyuqueue.el --- Emacs client for hyuqueue -*- lexical-binding: t -*-

;; Author: Logan
;; Version: 0.1.0
;; Package-Requires: ((emacs "28.1") (transient "0.4.0"))
;; Keywords: productivity, queue

;;; Commentary:

;; Emacs client for hyuqueue — a human work queue.
;;
;; Calls hyuqueue-server via HTTP and renders items using Emacs UI primitives:
;; - transient.el for the keyboard-driven activity palette (magit-style)
;; - completing-read / vertico for item selection
;;
;; This is the primary UI. The TUI exists to prove architecture is
;; editor-agnostic; day-to-day use happens here.
;;
;; Setup:
;;   (require 'hyuqueue)
;;   (setq hyuqueue-server-url "http://127.0.0.1:8731")
;;   (global-set-key (kbd "C-c h") #'hyuqueue)

;;; Code:

(require 'json)
(require 'url)
(require 'transient)

;; ── Configuration ─────────────────────────────────────────────────────────────

(defgroup hyuqueue nil
  "Emacs client for hyuqueue."
  :group 'tools
  :prefix "hyuqueue-")

(defcustom hyuqueue-server-url "http://127.0.0.1:8731"
  "Base URL of the running hyuqueue-server."
  :type 'string
  :group 'hyuqueue)

;; ── HTTP helpers ──────────────────────────────────────────────────────────────

(defun hyuqueue--get (path)
  "GET PATH from hyuqueue-server. Returns parsed JSON."
  (let* ((url (concat (string-trim-right hyuqueue-server-url "/") path))
         (url-request-method "GET")
         (url-request-extra-headers '(("Accept" . "application/json"))))
    (with-current-buffer (url-retrieve-synchronously url t)
      (goto-char (point-min))
      (re-search-forward "^$" nil t)
      (json-parse-buffer :object-type 'alist
                         :array-type 'list
                         :null-object nil))))

(defun hyuqueue--post (path &optional body)
  "POST BODY (alist) to PATH on hyuqueue-server. Returns parsed JSON."
  (let* ((url (concat (string-trim-right hyuqueue-server-url "/") path))
         (url-request-method "POST")
         (url-request-extra-headers
          '(("Content-Type" . "application/json")
            ("Accept" . "application/json")))
         (url-request-data
          (if body (encode-coding-string (json-encode body) 'utf-8) "{}")))
    (with-current-buffer (url-retrieve-synchronously url t)
      (goto-char (point-min))
      (re-search-forward "^$" nil t)
      (json-parse-buffer :object-type 'alist
                         :array-type 'list
                         :null-object nil))))

;; ── Core operations ───────────────────────────────────────────────────────────

(defun hyuqueue-next-item ()
  "Fetch the next item in the human queue."
  (alist-get 'item (hyuqueue--get "/api/v1/items/next")))

(defun hyuqueue-queue-count ()
  "Return the number of items in the human queue."
  (alist-get 'count (hyuqueue--get "/api/v1/items/count")))

(defun hyuqueue-ack (item-id)
  "Ack ITEM-ID — marks it done and advances the iron-mode queue."
  (hyuqueue--post (format "/api/v1/items/%s/ack" item-id)))

(defun hyuqueue-invoke-action (item-id activity-id &optional params)
  "Invoke ACTIVITY-ID on ITEM-ID with optional PARAMS alist."
  (hyuqueue--post
   (format "/api/v1/items/%s/action" item-id)
   `((activity_id . ,activity-id)
     (params . ,(or params '())))))

(defun hyuqueue-add-item (title source queue-id &optional body meta)
  "Add a new item to the queue."
  (hyuqueue--post
   "/api/v1/items"
   `((title . ,title)
     (source . ,source)
     (queue_id . ,queue-id)
     (body . ,body)
     (metadata . ,(or meta '())))))

;; ── Item display ──────────────────────────────────────────────────────────────

(defvar-local hyuqueue--current-item nil
  "The item currently displayed in the hyuqueue buffer.")

(defun hyuqueue--render-item (item count)
  "Render ITEM in the current buffer. COUNT is the queue depth."
  (let ((inhibit-read-only t))
    (erase-buffer)
    (if (null item)
        (insert (propertize "\nQueue is empty. Good job.\n"
                            'face '(:foreground "green" :weight bold)))
      (let* ((title (alist-get 'title item ""))
             (source (alist-get 'source item ""))
             (body (alist-get 'body item nil))
             (id (alist-get 'id item "")))
        ;; Header
        (insert (propertize
                 (format "hyuqueue  [%d in queue]\n\n" count)
                 'face '(:weight bold)))
        ;; Source + title
        (insert (propertize (format "[%s]" source)
                            'face '(:foreground "cyan")))
        (insert " ")
        (insert (propertize title 'face '(:weight bold)))
        (insert "\n\n")
        ;; Body
        (when body
          (insert body)
          (insert "\n\n"))
        ;; Item ID (small, for reference)
        (insert (propertize (format "id: %s\n" id)
                            'face '(:foreground "gray50" :height 0.8)))))))

;; ── Transient dispatch ────────────────────────────────────────────────────────

(transient-define-prefix hyuqueue-dispatch ()
  "Keyboard-driven activity palette for the current hyuqueue item."
  [:description
   (lambda ()
     (if hyuqueue--current-item
         (format "Item: %s" (alist-get 'title hyuqueue--current-item ""))
       "No item"))
   ;; Universal actions — always present
   [("SPC" "ack (iron mode gate)" hyuqueue--do-ack)
    ("r"   "refresh"              hyuqueue-refresh)
    ("q"   "quit"                 quit-window)]
   ;; Placeholder for item-scoped + global activities.
   ;; These will be populated dynamically from item.capabilities
   ;; and installed topic registrations in a future iteration.
   ["Other"
    ("?"   "show raw item JSON"   hyuqueue--show-raw)]])

(defun hyuqueue--do-ack ()
  "Ack the current item."
  (interactive)
  (when-let ((item hyuqueue--current-item))
    (hyuqueue-ack (alist-get 'id item ""))
    (message "Acked.")
    (hyuqueue-refresh)))

(defun hyuqueue--show-raw ()
  "Show the raw JSON of the current item in a separate buffer."
  (interactive)
  (when-let ((item hyuqueue--current-item))
    (let ((buf (get-buffer-create "*hyuqueue-raw*")))
      (with-current-buffer buf
        (erase-buffer)
        (insert (json-encode item))
        (json-pretty-print-buffer)
        (js-mode))
      (display-buffer buf))))

;; ── Main entry point ──────────────────────────────────────────────────────────

(defvar hyuqueue-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "SPC") #'hyuqueue--do-ack)
    (define-key map (kbd "r")   #'hyuqueue-refresh)
    (define-key map (kbd "?")   #'hyuqueue-dispatch)
    (define-key map (kbd "q")   #'quit-window)
    map)
  "Keymap for hyuqueue-mode.")

(define-derived-mode hyuqueue-mode special-mode "hyuqueue"
  "Major mode for the hyuqueue item buffer."
  :group 'hyuqueue
  (setq buffer-read-only t))

(defun hyuqueue-refresh ()
  "Refresh the current item and count from the server."
  (interactive)
  (let ((item (hyuqueue-next-item))
        (count (hyuqueue-queue-count)))
    (setq hyuqueue--current-item item)
    (hyuqueue--render-item item count)
    (message "hyuqueue: %d item(s) in queue" count)))

;;;###autoload
(defun hyuqueue ()
  "Open the hyuqueue item buffer."
  (interactive)
  (let ((buf (get-buffer-create "*hyuqueue*")))
    (with-current-buffer buf
      (hyuqueue-mode)
      (hyuqueue-refresh))
    (switch-to-buffer buf)))

(provide 'hyuqueue)
;;; hyuqueue.el ends here
