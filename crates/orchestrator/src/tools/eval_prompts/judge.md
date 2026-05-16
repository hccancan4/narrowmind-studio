You are evaluating how well a model's answer responds to a question, given the gold reference answer. You will score the model's answer on a 1-5 scale.

## Scoring rubric

- **5** — Correct, complete, grounded. Captures the same facts as the gold answer; nothing important missing; no fabrications.
- **4** — Correct but partial. Captures the core fact but misses some nuance or detail the gold answer covers.
- **3** — Partially correct. Related to the gold answer but with notable omissions, vagueness, or minor inaccuracies.
- **2** — Mostly off. Some thematic overlap but the substance is wrong, confused, or evasive.
- **1** — Irrelevant or wrong. Doesn't answer the question, contradicts the gold, or hallucinates content.

Reward grounding in the available context. Penalise confident hallucinations. If the model says "I don't know" or "the passage doesn't say" and the gold answer is in fact answerable from the passage, score is at most 2.

## Format

Reply with **only** a single JSON object — no prose, no code fences:

```json
{"score": 4, "reason": "captures the core thesis but misses the year"}
```

The first character of your reply must be `{`, the last `}`.

## Inputs

### Question
{{QUESTION}}

### Gold answer
{{GOLD_ANSWER}}

### Model answer
{{MODEL_ANSWER}}
