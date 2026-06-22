You are generating supervised fine-tuning (SFT) data for a domain-specific language model that answers questions using retrieved context. Read the passage below and produce {{N_PAIRS}} high-quality question + answer pairs grounded entirely in the passage. These answers teach the model how to respond — so make them **complete, accurate, and self-contained**.

## Output format

Return **only** a JSON array. Each element is an object with two string fields: `question` and `answer`. No prose before or after. No code fences. No explanation. The first character of your reply must be `[`. The last character must be `]`.

Example shape (do not copy contents):

[
  {"question": "...", "answer": "..."},
  {"question": "...", "answer": "..."}
]

## Quality rules

- Each `question` must be answerable from the passage alone. Do not require outside knowledge.
- Each `answer` must be **complete and self-contained**: 2–6 sentences that fully resolve the question — include the key reasoning, conditions, or distinctions the passage makes, not just a one-line definition. A reader who cannot see the passage should come away with the full, correct picture. Stay focused on the question; do not pad or dump the whole passage.
- **Accuracy above all.** When the passage is subtle (named positions, careful distinctions, necessary conditions), preserve it precisely — never flatten it into something the passage does not say. A confident but wrong answer is worse than a careful one.
- Write in fluent prose; paraphrase and **synthesize**. Quote verbatim only when a term is essential — do not copy whole sentences from the passage wholesale.
- Vary question type: definition / mechanism / comparison / example / consequence. Avoid asking the same thing twice.
- Skip the passage entirely (and return `[]`) if it is not informative — e.g. just navigation, table of contents, citations, or fragments.
- Do not invent statistics, dates, proper nouns, or claims the passage does not contain.
- Do not start questions with "According to the passage" — write them as if a curious student were asking. The grounding is implicit from the dataset.

## Passage

{{CHUNK_TEXT}}
