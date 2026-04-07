;;; hyuqueue-tests.el --- ERT tests for hyuqueue.el -*- lexical-binding: t -*-

;;; Commentary:

;; Tests for hyuqueue rendering logic.  No server required — these
;; exercise pure functions that operate on alist data.

;;; Code:

(require 'ert)
(require 'hyuqueue)

;; ── hyuqueue--render-item ───────────────────────────────────────────────────

(ert-deftest hyuqueue-render-item/nil-item-shows-empty ()
  "A nil item (empty queue) renders the empty-queue message."
  (with-temp-buffer
    (hyuqueue--render-item nil 0)
    (should (string-match-p "Queue is empty" (buffer-string)))))

(ert-deftest hyuqueue-render-item/full-item-renders-all-fields ()
  "An item with every field populated renders title, source, body, and id."
  (let ((item '((id . "abc-123")
                (title . "Review PR #42")
                (source . "github")
                (body . "Looks good to me."))))
    (with-temp-buffer
      (hyuqueue--render-item item 3)
      (let ((text (buffer-string)))
        (should (string-match-p "\\[github\\]" text))
        (should (string-match-p "Review PR #42" text))
        (should (string-match-p "Looks good to me\\." text))
        (should (string-match-p "id: abc-123" text))
        (should (string-match-p "3 in queue" text))))))

(ert-deftest hyuqueue-render-item/nil-body-omitted ()
  "An item whose body is nil should render without a body section."
  (let ((item '((id . "def-456")
                (title . "Empty body task")
                (source . "email")
                (body . nil))))
    (with-temp-buffer
      (hyuqueue--render-item item 1)
      (let ((text (buffer-string)))
        (should (string-match-p "Empty body task" text))
        (should-not (string-match-p "nil" text))))))

(ert-deftest hyuqueue-render-item/missing-fields-use-defaults ()
  "An item missing title, source, and body should not crash.
This guards against `:null' leaking from JSON parsing."
  (let ((item '((id . "ghi-789"))))
    (with-temp-buffer
      (hyuqueue--render-item item 0)
      (let ((text (buffer-string)))
        (should (string-match-p "id: ghi-789" text))
        ;; Defaults are empty strings — no `:null' in the output.
        (should-not (string-match-p ":null" text))))))

;;; hyuqueue-tests.el ends here
