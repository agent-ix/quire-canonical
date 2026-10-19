# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 Agent-IX
"""quire-canonical's documentation figures, rendered by the docs-figures skill.

Regenerate from the repository root with the agent-ix/dev-tools skill:

    python3 -m pip install fonttools
    python3 <dev-tools>/skills/docs-figures/scripts/render.py docs/images/figures.py

Add --check to confirm the committed SVGs are current. The digest in the hero
figure is the SHA-256 of the canonical text shown there.
"""

import hashlib

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


CANONICAL = b'{"a":1,"b":2.5}'
_hex = hashlib.sha256(CANONICAL).hexdigest()
DIGEST = f"{_hex[:8]}…{_hex[-7:]}"


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
    f.box(540, 80, 190, 56, "model", "canonical bytes", CANONICAL.decode())
    f.box(800, 80, 180, 56, "evidence", "SHA-256", DIGEST)
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
    (1, 0, "quire-driver", "the quire command", True),
    (2, 0, "quire-integration", "tests the whole chain", True),
    (3, 0, "quire-verification", "plans verification", True),
    (0, 1, "quire-specification", "the standard", True),
    (1, 1, "quire-spec-language", "compiler · evaluator", False),
    (2, 1, "quire-contract-ir", "contract model", False),
    (3, 1, "quire-contract-codegen", "code · tests · proofs", False),
    (4, 1, "quire-contract-runtime", "runs generated checks", False),
    (0, 2, "quire-observation", "qualified observations", False),
    (1, 2, "quire-protocol", "protocol conformance", True),
    (2, 2, "quire-analyze", "SMT consistency", False),
    (0, 3, "quire-mltl", "MLTL over observations", False),
]
FEEDS = [
    ("quire-specification", "quire-spec-language"),
    ("quire-spec-language", "quire-contract-ir"),
    ("quire-contract-ir", "quire-contract-codegen"),
    ("quire-contract-codegen", "quire-contract-runtime"),
    ("quire-spec-language", "quire-protocol"),
    ("quire-contract-ir", "quire-analyze"),
    ("quire-observation", "quire-protocol"),
    ("quire-observation", "quire-mltl"),
]
SHARED = ["quire-exact", "quire-semantic-value", "quire-walk", "quire-canonical",
          "quire-verification-contracts"]


def ecosystem(here="quire-spec-language"):
    """The Quire repositories: the main chain, its branches, and what runs and shares it."""
    f = Figure(
        "ecosystem",
        980,
        476,
        "The Quire repositories. The main chain runs from the standard, quire-specification, to "
        "the compiler, quire-spec-language, then quire-contract-ir, quire-contract-codegen and "
        "quire-contract-runtime. quire-spec-language also feeds quire-protocol, and "
        "quire-contract-ir feeds quire-analyze. quire-observation feeds quire-protocol and "
        "quire-mltl. Above the chain, quire-driver runs it, quire-integration tests it and "
        "quire-verification plans verification. Below it, shared libraries used throughout: "
        "quire-exact, quire-semantic-value, quire-walk, quire-canonical and "
        "quire-verification-contracts.",
    )
    w, h, step = 176, 50, 201
    rows = {0: 20, 1: 120, 2: 210, 3: 290}
    where = {}
    for col, row, name, sub, private in REPOS:
        x, y = col * step, rows[row]
        where[name] = (x, y)
        role = "logic" if name == here else "ink"
        f.box(x, y, w, h, role, name, sub, dashed=private)
    for a, b in FEEDS:
        (ax, ay), (bx, by) = where[a], where[b]
        if ay == by:
            f.line([(ax + w, ay + h / 2), (bx - 3, by + h / 2)], "model")
        else:
            f.line([(ax + w / 2, ay + h), (bx + w / 2, by - 3)], "model")
    f.text(0, 12, "ORCHESTRATION", "muted", "mono", SMALL)
    f.text(0, 112, "THE CHAIN", "muted", "mono", SMALL)
    f.text(0, 382, "SHARED LIBRARIES · USED THROUGHOUT", "muted", "mono", SMALL)
    gap = 10
    widths = [FONTS["title"].width(name, TITLE) + 24 for name in SHARED]
    spare = (980 - sum(widths) - gap * (len(SHARED) - 1)) / len(SHARED)
    x = 0
    for name, w0 in zip(SHARED, widths):
        w0 = round(w0 + spare)
        f.box(x, 390, w0, h, "logic" if name == here else "ink", name)
        x += w0 + gap
    f.legend(0, 472, [("logic", "this repository", "box"), ("ink", "private", "dash"),
                      ("model", "feeds", "fill")])
    return f


def this_repo():
    """The ecosystem figure with this repository highlighted."""
    return ecosystem(here="quire-canonical")


FIGURES = [hero, pipeline, this_repo]
