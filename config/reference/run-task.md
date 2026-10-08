---
name: run-task
description: "Execute a task following appropriate rules with rule-advisor metacognition. Use when executing a specific task, running an implementation task, or when 'run-task' is mentioned."
when: "Executing a specific task, running an implementation task, or when 'run-task' is mentioned."
keywords: [run-task, run, task, execute, following, appropriate, rules, rule-advisor, metacognition, executing, specific, running]
---
# Task Execution with Metacognitive Analysis

Task: $ARGUMENTS

## Mandatory Execution Process

**Step 1: Rule Selection via rule-advisor (REQUIRED)**

Invoke a rule-advisor using Agent tool:
- `subagent_type`: "general-purpose"
- `description`: "Rule selection"
- `prompt`: "You are a rule advisor. Task: $ARGUMENTS. Select the applicable rules (repo AGENTS.md/CLAUDE.md, and skills from `bn skills find <task words>`) and perform metacognitive analysis. Return JSON: taskAnalysis.essence, selectedRules (rule or skill name plus the concrete procedure), metaCognitiveGuidance.pastFailures, metaCognitiveGuidance.firstStep, warningPatterns."

**Step 2: Utilize rule-advisor Output**

1. **Understand Task Essence** (`taskAnalysis.essence`): the fundamental purpose; a "quick fix" vs a "proper solution".
2. **Follow Selected Rules** (`selectedRules`): apply each one's concrete procedures.
3. **Recognize Past Failures** (`metaCognitiveGuidance.pastFailures`): apply countermeasures and suggested alternatives.
4. **Execute First Action** (`metaCognitiveGuidance.firstStep`): start with the recommended action and tools.

**Step 3: Create Task List with TaskCreate**

Register work steps using TaskCreate. Always include: first "Confirm skill constraints", final "Verify skill fidelity".

Break down the task based on rule-advisor's guidance:
- Reflect `taskAnalysis.essence` in task descriptions
- Apply `metaCognitiveGuidance.firstStep` to first task
- Restructure tasks considering `warningPatterns`
- Set priorities based on dependency order and warningPatterns severity

**Step 4: Execute Implementation**

- Start with the `metaCognitiveGuidance.firstStep` action
- Follow the selected rules and their quality standards; keep the task list current with TaskUpdate
- Monitor warningPatterns throughout execution and adjust the approach when one triggers
- Done when every task is complete and the final "Verify skill fidelity" task has passed
