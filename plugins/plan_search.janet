# PlanSearch-lite — a `/plan <task>` command that runs a task with
# diverse natural-language planning BEFORE any code, then implements the
# best approach (and backtracks to a different one on a dead end).
#
# Based on "Planning In Natural Language Improves LLM Search For Code
# Generation" (PlanSearch), arXiv:2409.03733. The paper's finding: models
# repeatedly sample similar (often wrong) code; searching over *diverse
# natural-language plans* first dramatically improves success
# (77% vs 41% pass@1 on LiveCodeBench). dirge plugins can't make their own
# LLM calls, so this is implemented as prompt steering: the command
# composes a Plan-Search prompt and queues it as the next turn via
# `harness/request-prompt`; the main agent does the planning + work.
#
# Install: copy to ~/.config/dirge/plugins/ (or ./.dirge/plugins/).
# Usage:   /plan <task>

(def plansearch-preamble
  (string
    "Use a Plan-Search approach for this task. Before writing ANY code:\n\n"
    "1. OBSERVE — list 4 distinct, factual observations about the problem: "
    "constraints, edge cases, what makes it tricky, relevant facts. Not solutions yet.\n"
    "2. APPROACHES — from those observations, derive 3-5 GENUINELY DIFFERENT "
    "approaches (different data structures, algorithms, or decompositions — not "
    "variations of one idea). For each: one line on the core idea and its main risk.\n"
    "3. CHOOSE — pick the most promising approach and justify in 1-2 sentences. "
    "Prefer the simplest one that handles the observed constraints.\n"
    "4. IMPLEMENT — execute the chosen plan and verify it. If it hits a dead end, "
    "go back to step 2 and try a DIFFERENT approach rather than forcing the first one.\n"))

(defn plan-handler [args]
  (def task (if (string? args) (string/trim args) ""))
  (if (= task "")
    (string "usage: /plan <task>\n"
            "Runs the task with Plan-Search — diverse approaches first, then "
            "implement the best one (backtracking to another on a dead end).")
    (do
      (harness/request-prompt (string plansearch-preamble "\nTask: " task))
      (string "Plan-Search engaged for: " task
              "\n(generating diverse approaches before implementing…)"))))

(harness/register-command "plan" "plan-handler")
