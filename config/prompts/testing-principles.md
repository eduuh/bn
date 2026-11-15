---
name: testing-principles
description: "Language-agnostic testing principles: TDD, test quality, coverage, test design. Use when writing tests, planning a test strategy or reviewing test quality."
when: Writing tests, designing test strategies, or reviewing test quality.
keywords: [testing-principles, testing, principles, language-agnostic, including, tdd, test, quality, coverage, standards, design, patterns]
---
1. Read the repository's existing test strategy and affected test suite.
2. Load `bn skills render testing-principles-reference` for the full standards.
3. Test the introduced or fixed behavior and meaningful edge cases. Use a failing-before/
   passing-after check when practical; do not restate constants or framework behavior,
   invent coverage thresholds, or fix unrelated tests.
4. Run the smallest existing checks covering the change and widen only when failures,
   risk, or repository policy require it.
