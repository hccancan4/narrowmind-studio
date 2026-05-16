You are generating supervised fine-tuning (SFT) data for a domain-specific language model. Read the passage below and produce {{N_PAIRS}} high-quality question + answer pairs grounded entirely in the passage.

## Output format

Return **only** a JSON array. Each element is an object with two string fields: `question` and `answer`. No prose before or after. No code fences. No explanation. The first character of your reply must be `[`. The last character must be `]`.

Example shape (do not copy contents):

[
  {"question": "...", "answer": "..."},
  {"question": "...", "answer": "..."}
]

## Quality rules

- Each `question` must be answerable from the passage alone. Do not require outside knowledge.
- Each `answer` must be 1–4 sentences, written in fluent prose, and factually grounded in the passage. Quote phrases verbatim only when essential; otherwise paraphrase cleanly.
- Vary question type: definition / mechanism / comparison / example / consequence. Avoid asking the same thing twice.
- Skip the passage entirely (and return `[]`) if it is not informative — e.g. just navigation, table of contents, citations, or fragments.
- Do not invent statistics, dates, or proper nouns the passage does not contain.
- Do not start questions with "According to the passage" — write them as if a curious student were asking. The grounding is implicit from the dataset.

## Passage

{{CHUNK_TEXT}}
