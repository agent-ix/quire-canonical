# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 Agent-IX
"""quire-canonical's documentation figures, rendered by the docs-figures skill.

Regenerate from the repository root with the agent-ix/dev-tools skill:

    python3 -m pip install fonttools
    python3 <dev-tools>/skills/docs-figures/scripts/render.py docs/images/figures.py

Add --check to confirm the committed SVGs are current. The digest in the hero
figure is the SHA-256 of the canonical text shown there.
"""

from docs_figures import FONTS, SMALL, TITLE, Figure  # noqa: F401

STYLE = "ix-docs"

# One role per kind of thing, fixed across the Quire repositories.
ROLES = [
    ("code", "authored source", "box"),
    ("logic", "compiler stage", "box"),
    ("model", "checked package / IR", "box"),
    ("evidence", "outcome", "box"),
    ("ink", "input", "box"),
]


def hero():
    """README hero: two spellings of one value, one canonical text, one digest."""
    f = Figure(
        "hero",
        980,
        250,
        "Two JSON texts that differ in key order and number spelling, {\"b\": 2.50, \"a\": 1.0} "
        "and {\"a\": 1, \"b\": 2.5e0}, are both read and encoded to the same canonical bytes, "
        "{\"a\":1,\"b\":2.5}, and so to the same SHA-256 digest.",
    )
    f.box(0, 30, 230, 56, "ink", "JSON text", '{"b": 2.50, "a": 1.0}')
    f.box(0, 130, 230, 56, "ink", "same value, other spelling", '{"a": 1, "b": 2.5e0}')
    f.box(300, 80, 170, 56, "logic", "quire-canonical", "sort · number text")
    f.box(540, 80, 190, 56, "model", "canonical bytes", '{"a":1,"b":2.5}')
    f.box(800, 80, 180, 56, "evidence", "SHA-256", "85f690f6…9050629")
    f.line([(230, 58), (265, 58), (265, 100), (297, 100)], "ink")
    f.line([(230, 158), (265, 158), (265, 116), (297, 116)], "ink")
    f.line([(470, 108), (537, 108)], "logic")
    f.line([(730, 108), (797, 108)], "model")
    f.note(332, 168, "RFC 8785", "muted")
    f.legend(0, 244, [ROLES[4], ("logic", "encoder", "box"), ("model", "canonical text", "box"), ("evidence", "digest", "box")])
    return f


def pipeline():
    """Every way in, one writer, every way out."""
    f = Figure(
        "pipeline",
        980,
        330,
        "A value reaches the Writer as pushed events, as a Document from read, as a "
        "serde_json::Value or through serde for a FixedShape type. The Writer emits canonical "
        "bytes in order into a Sink: a SHA-256 hasher, a byte vector or an io::Write.",
    )
    ys = [20, 100, 180, 260]
    ins = [
        ("pushed events", "your own stack"),
        ("Document", "from read(bytes)"),
        ("serde_json::Value", "feature serde_json"),
        ("FixedShape type", "serde, fixed depth"),
    ]
    for y, (a, b) in zip(ys, ins):
        f.box(0, y - 20, 210, 56, "ink", a, b)
    f.box(330, 120, 170, 56, "logic", "Writer", "events in order")
    for y in ys:
        f.line([(210, y + 8), (270, y + 8), (270, 148), (327, 148)], "ink")
    f.box(580, 120, 150, 56, "model", "canonical bytes", "no whole String")
    f.line([(500, 148), (577, 148)], "logic")
    sinks = [
        (30, "SHA-256 hasher", "digest"),
        (120, "Vec<u8>", "to_vec"),
        (210, "io::Write", "WriteSink · std"),
    ]
    f.line([(730, 148), (760, 148)], "model", arrow=False)
    f.line([(760, 58), (760, 238)], "model", arrow=False)
    for y, a, b in sinks:
        f.line([(760, y + 28), (797, y + 28)], "model")
        f.box(800, y, 180, 56, "evidence", a, b)
    f.legend(0, 324, [ROLES[4], ("logic", "encoder", "box"), ("model", "canonical text", "box"), ("evidence", "sink", "box")])
    return f


REPOS = [
    ("ORCHESTRATION", [("quire-verification", True), ("quire-driver", True), ("quire-integration", True)]),
    ("BACKENDS", [("quire-contract-codegen", False), ("quire-contract-runtime", False),
                  ("quire-analyze", False), ("quire-protocol", True), ("quire-mltl", False)]),
    ("LANGUAGE", [("quire-specification", True), ("quire-spec-language", False), ("quire-contract-ir", False)]),
    ("FOUNDATIONS", [("quire-exact", False), ("quire-semantic-value", False), ("quire-walk", False),
                     ("quire-canonical", False), ("quire-verification-contracts", False),
                     ("quire-observation", False)]),
]


def ecosystem(here="quire-spec-language"):
    """The Quire repositories in bands; each band builds on the ones below."""
    f = Figure(
        "ecosystem",
        980,
        364,
        "The Quire repositories in four bands. Foundations: quire-exact, quire-semantic-value, "
        "quire-walk, quire-canonical, quire-verification-contracts, quire-observation. Language: "
        "quire-specification, quire-spec-language, quire-contract-ir. Backends: "
        "quire-contract-codegen, quire-contract-runtime, quire-analyze, quire-protocol, "
        "quire-mltl. Orchestration: quire-verification, quire-driver, quire-integration.",
    )
    y = 0
    gap = 10
    for band, repos in REPOS:
        f.text(0, y + 12, band, "muted", "mono", SMALL)
        widths = [FONTS["title"].width(name, TITLE) + 24 for name, _ in repos]
        spare = (980 - sum(widths) - gap * (len(repos) - 1)) / len(repos)
        x = 0
        for (name, private), w in zip(repos, widths):
            w = round(w + spare)
            role = "logic" if name == here else "ink"
            f.box(x, y + 20, w, 50, role, name, "private" if private else "public", dashed=private)
            x += w + gap
        y += 88
    f.legend(0, 360, [("logic", "this repository", "box"), ("ink", "private", "dash")])
    return f


def this_repo():
    """The ecosystem figure with this repository highlighted."""
    return ecosystem(here="quire-canonical")


FIGURES = [hero, pipeline, this_repo]
