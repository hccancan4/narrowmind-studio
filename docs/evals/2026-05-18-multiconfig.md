# NarrowMind RAG — multi-config eval (Phase 3.5)

Side-by-side comparison of three retrieval modes against the same
56-pair eval set (46 random + 10 proper-noun-targeted). Same Qwen2.5-7B
GGUF, same top_k=5, same Sonnet judge.


## Aggregate

| metric | dense | sparse | hybrid | Phase 3 baseline (19 pairs) |
|---|---:|---:|---:|---:|
| retrieval recall@5  | 0.89 | 0.98 | **0.98** | 0.79 |
| LLM-judge mean      | 4.23 | 4.43 | **4.55** | 3.37 |
| judge score = 5    | 28 | 32 | 35 | — |
| judge score = 4    | 20 | 18 | 17 | — |
| judge score = 3    | 4 | 5 | 4 | — |
| judge score = 2    | 1 | 0 | 0 | — |
| judge score = 1    | 3 | 1 | 0 | — |

## Phase 4 gating thresholds

| threshold | required | hybrid actual | verdict |
|---|---|---|---|
| recall@5 >= 0.85       | 0.85 | 0.98 | PASS |
| judge mean >= 3.8     | 3.80 | 4.55 | PASS |

**Both thresholds passed -> Phase 4 (LoRA fine-tune) is unblocked.**


## Dense -> Hybrid delta

| change | pairs |
|---|---:|
| recall flipped MISS -> HIT in hybrid | 5 |
| recall flipped HIT -> MISS in hybrid | 0 |
| judge score increased in hybrid     | 12 |
| judge score decreased in hybrid     | 4 |

## Per-pair score matrix

Recall ✓/✗ then judge score (1-5). Cells where hybrid beats dense are bolded.


| # | dense | sparse | hybrid | question |
|---:|:---:|:---:|:---:|---|
| 1 | ✓5 | ✓3 | ✓5 | What is meant by 'mentalese,' and how does it relate to spoken languages |
| 2 | ✓3 | ✓3 | ✓3 | How does the Madhyamaka view treat the relationship between mental and p |
| 3 | ✓4 | ✓5 | **✓5** | How does Kant use the notion of amphiboly in his discussion of the conce |
| 4 | ✓4 | ✓4 | ✓4 | How does the Epimenides-style statement 'Lucas can't assert the truth of |
| 5 | ✓4 | ✓3 | ✓4 | How does Ramanuja's Vishishtadvaita differ from Advaita Vedanta in its c |
| 6 | ✓4 | ✓5 | ✓3 | How did Henry Denifle's research reshape the scholarly understanding of  |
| 7 | ✓4 | ✓5 | **✓5** | What does the law of form perception about objects moving in the same di |
| 8 | ✗4 | ✓5 | **✓5** | How do Rey and Devitt defend eliminativism against the charge of self-re |
| 9 | ✓4 | ✓4 | ✓4 | What are the three basic mindsets proposed by the Triune Ethics Meta-the |
| 10 | ✓4 | ✓3 | **✓5** | What is the explanatory gap in philosophy of mind, and how do Chalmers,  |
| 11 | ✓5 | ✓5 | ✓5 | What is the 'dogma of harmony' criticism leveled against 4E cognition th |
| 12 | ✗2 | ✓5 | **✓5** | What is Daniel Dennett's argument about natural selection and consciousn |
| 13 | ✓4 | ✓5 | ✓4 | How does Alois M. Haas argue against describing Eckhart's 'breakthrough' |
| 14 | ✓4 | ✓4 | **✓5** | How does moral satisficing differ from theories like Hauser's 'moral gra |
| 15 | ✓5 | ✓5 | ✓5 | What is the Stoic concept of the hēgemonikón, and where did the Stoics l |
| 16 | ✓4 | ✓5 | ✓4 | How did Alexander Gottlieb Baumgarten conceive of the 'ground of the sou |
| 17 | ✓5 | ✓5 | ✓5 | What are the six levels of moral functioning identified in the integrate |
| 18 | ✓5 | ✓4 | ✓5 | How does Eckhart use the idea that nature abhors a vacuum to explain God |
| 19 | ✓4 | ✓4 | ✓4 | What distinction does Eckhart draw between a 'master of reading' and a ' |
| 20 | ✓5 | ✓5 | ✓5 | What is the Ground of the Soul, and who originated the concept? |
| 21 | ✓4 | ✓4 | **✓5** | How does Schnädelbach characterize reflection's role in modern philosoph |
| 22 | ✓5 | ✓5 | ✓5 | What distinguishes moral reasoning from moral development as areas of st |
| 23 | ✓4 | ✓4 | ✓4 | What is the role of the "One in us" in Proclus' theory of how the soul c |
| 24 | ✓5 | ✓5 | ✓5 | Why do symbolic motifs like fractals, cycles, and dualities appear repea |
| 25 | ✓5 | ✓5 | ✓5 | What is epiphenomenalism, and how did Thomas Huxley illustrate it? |
| 26 | ✓5 | ✓5 | ✓5 | What is the theory of neural reuse, and which works by Anderson are most |
| 27 | ✓3 | ✓4 | **✓4** | Why do eliminativists think the intuitive plausibility of folk psycholog |
| 28 | ✓4 | ✓4 | ✓3 | How does Mojsisch connect Eckhart's idea of the soul's ground to Fichte' |
| 29 | ✓4 | ✓4 | ✓4 | How does functionalism in psychology view the psyche, and what later mov |
| 30 | ✓5 | ✓5 | ✓5 | What are the three fundamental principles of theology as a science accor |
| 31 | ✓5 | ✓5 | ✓5 | How does Bavinck understand the role of proofs for God's existence, and  |
| 32 | ✓4 | ✓4 | ✓4 | How does occasionalism explain the apparent causal relationships between |
| 33 | ✓5 | ✓5 | ✓5 | How did Augusto Blasi's self-model attempt to address the gap researcher |
| 34 | ✓5 | ✓5 | ✓5 | What is interactionist dualism, and what is the central problem it faces |
| 35 | ✓4 | ✓4 | ✓4 | What is the title and publisher of Kate Crawford's 2021 book on artifici |
| 36 | ✓5 | ✓5 | ✓5 | How did the Stoics conceptualize the human soul in relation to the cosmo |
| 37 | ✓5 | ✓5 | ✓5 | What distinguishes philosophy of psychology from theoretical psychology, |
| 38 | ✓3 | ✓5 | **✓4** | How does Eckhart distinguish between God as a person and the Godhead, an |
| 39 | ✓4 | ✓4 | ✓4 | How did John of the Cross describe the ground of the soul and God's pres |
| 40 | ✓5 | ✓5 | ✓4 | How does Dennett's "Quinian crossword puzzle" analogy address the indete |
| 41 | ✓4 | ✓4 | ✓4 | What is the Chinese Room thought experiment, and what conclusion does Se |
| 42 | ✓5 | ✓4 | ✓5 | How does Karl Heinz Witte characterize Eckhart's view of the individual  |
| 43 | ✓5 | ✓5 | ✓5 | What is open individualism as described by Daniel Kolak, and how does it |
| 44 | ✓5 | ✓5 | ✓5 | What is psychophysical parallelism, and how did Leibniz reconcile it wit |
| 45 | ✓5 | ✓5 | ✓5 | Why did some Seceders refuse to join the merger that formed the Gereform |
| 46 | ✓5 | ✓5 | ✓5 | What is George Kelly's fundamental postulate in Personal Construct Theor |
| 47 | ✗1 | ✓5 | **✓5** | What was the title of C. D. Broad's 1930 book published by Harcourt, Bra |
| 48 | ✗3 | ✓3 | ✗3 | What were the Stone Lectures that Herman Bavinck delivered, and when did |
| 49 | ✗1 | ✓4 | **✓4** | What is the title of Frank Jackson's 1982 paper on qualia? |
| 50 | ✓5 | ✓4 | ✓5 | What book did Hubert Dreyfus publish in 1972 about computer limitations? |
| 51 | ✓5 | ✗1 | ✓5 | What was the original Dutch title of Herman Bavinck's Reformed Dogmatics |
| 52 | ✓5 | ✓5 | ✓5 | What article did Herman Bavinck publish in The Presbyterian and Reformed |
| 53 | ✗1 | ✓5 | **✓5** | What did Carolyn Merchant argue about Anne Conway's influence on Leibniz |
| 54 | ✓5 | ✓5 | ✓5 | Who did Lilli Alanen marry in 1992? |
| 55 | ✓5 | ✓4 | ✓4 | What is the title of Berit Brogaard's 2009 article published in Philosop |
| 56 | ✓5 | ✓5 | ✓5 | Who were the editors of 'Niels Bohr and Philosophy of Physics: Twenty Fi |

## Residual hybrid failures (judge <= 2)

None. Every pair scored 3+ under hybrid retrieval.


---

- dense report:  `5290567afaec4787a03455bb18711786-dense.md`
- sparse report: `03294f47fdb94103a7b0e8a07897065c-sparse.md`
- hybrid report: `988c445fc100405386fe827dcdde8123-hybrid.md`