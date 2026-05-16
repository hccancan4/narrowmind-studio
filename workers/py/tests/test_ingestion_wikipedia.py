"""Wikipedia category traversal — pure-logic tests with a fake category graph.

We mock the wikipedia-api ``WikipediaPage`` interface enough to exercise the BFS walker.
Live Wikipedia is covered by the Phase 2 acceptance run, not by unit tests."""

from __future__ import annotations

from dataclasses import dataclass, field

from narrowmind_workers.ingestion.wikipedia import (
    WIKIPEDIA_NAMESPACE_ARTICLE,
    WIKIPEDIA_NAMESPACE_CATEGORY,
    collect_category_pages,
    user_agent,
)


@dataclass
class FakePage:
    title: str
    ns: int
    members: dict[str, "FakePage"] = field(default_factory=dict)

    @property
    def categorymembers(self) -> dict[str, "FakePage"]:
        return self.members


def _article(title: str) -> FakePage:
    return FakePage(title=title, ns=WIKIPEDIA_NAMESPACE_ARTICLE)


def _category(title: str, members: dict[str, FakePage]) -> FakePage:
    return FakePage(title=title, ns=WIKIPEDIA_NAMESPACE_CATEGORY, members=members)


def test_collect_pages_respects_max_pages_cap() -> None:
    root = _category("Root", {
        "A": _article("A"), "B": _article("B"), "C": _article("C"),
    })
    pages = collect_category_pages(root, max_depth=0, max_pages=2)
    assert [p.title for p in pages] == ["A", "B"]


def test_collect_pages_descends_into_subcategories_when_depth_allows() -> None:
    root = _category("Root", {
        "Topic A": _article("Topic A"),
        "Sub1": _category("Sub1", {
            "Topic B": _article("Topic B"),
            "Topic C": _article("Topic C"),
        }),
    })
    pages = collect_category_pages(root, max_depth=1, max_pages=10)
    titles = {p.title for p in pages}
    assert titles == {"Topic A", "Topic B", "Topic C"}


def test_collect_pages_stops_at_max_depth() -> None:
    root = _category("Root", {
        "Top": _article("Top"),
        "Sub1": _category("Sub1", {
            "Mid": _article("Mid"),
            "Sub2": _category("Sub2", {
                "Deep": _article("Deep"),
            }),
        }),
    })
    pages = collect_category_pages(root, max_depth=1, max_pages=10)
    # Sub1's articles included; Sub2 (depth 2) not descended.
    assert {p.title for p in pages} == {"Top", "Mid"}


def test_collect_pages_dedupes_articles_across_subcategories() -> None:
    repeated = _article("Repeat")
    root = _category("Root", {
        "Sub1": _category("Sub1", {"Repeat": repeated}),
        "Sub2": _category("Sub2", {"Repeat": repeated}),
    })
    pages = collect_category_pages(root, max_depth=1, max_pages=10)
    titles = [p.title for p in pages]
    assert titles == ["Repeat"]


def test_user_agent_includes_repo_url_and_version() -> None:
    ua = user_agent()
    assert "NarrowMindStudio/" in ua
    assert "github.com/hccancan4/narrowmind-studio" in ua
