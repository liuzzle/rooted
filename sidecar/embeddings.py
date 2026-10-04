"""
Vectors for search by meaning — local, optional, and never the only path.

Search by words always works; this adds the passages that say the same thing
in other words ("Gnade" finding a note about "unverdiente Gunst"). It runs
only when a local Ollama server has the embedding model, because the notes are
the most sensitive data in the app and nothing about them is sent anywhere.

What a vector is *for* matters here: it ranks passages that already exist. It
is never turned back into text, and nothing it scores is shown except the
passage itself — so a poor model makes search worse, never untruthful.

Vectors are stored as little-endian float32, unit length, so a dot product is
the cosine. Each one records the model that made it: two models' vectors are
not comparable, and the query has to be embedded by the same model as the text
it is compared against.
"""

from __future__ import annotations

import json
import math
import os
import struct
import urllib.error
import urllib.request
from typing import Optional, Sequence

OLLAMA_HOST = os.environ.get("ROOTED_OLLAMA_HOST", "http://localhost:11434")
# Multilingual, because the notes are German and the packs are German and
# English. A model that only knew English would place every German passage
# near every other.
EMBED_MODEL = os.environ.get("ROOTED_EMBED_MODEL", "bge-m3")

# Texts per request. Big enough to amortise the round trip, small enough that
# one batch never holds the database for long.
BATCH = 32


class EmbeddingUnavailable(RuntimeError):
    """No local model answered. Ordinary — search by words still works."""


def installed_models(host: Optional[str] = None,
                     timeout: float = 1.5) -> list[str]:
    try:
        with urllib.request.urlopen(f"{host or OLLAMA_HOST}/api/tags",
                                    timeout=timeout) as resp:
            listed = json.load(resp).get("models", [])
    except (urllib.error.URLError, OSError, ValueError):
        return []
    return [m.get("name", "") for m in listed]


def model_available(model: Optional[str] = None,
                    host: Optional[str] = None) -> bool:
    """Whether the server is up *and* has this model pulled.

    Ollama names carry a tag ("bge-m3:latest"); a bare name means `latest`.
    """
    name = model or EMBED_MODEL
    wanted = {name} if ":" in name else {name, f"{name}:latest"}
    return any(m in wanted for m in installed_models(host))


def embed(texts: Sequence[str], model: Optional[str] = None,
          host: Optional[str] = None, timeout: float = 120.0) -> list[list[float]]:
    """Embed a batch, returning unit vectors in the same order."""
    if not texts:
        return []
    body = json.dumps({"model": model or EMBED_MODEL, "input": list(texts)})
    req = urllib.request.Request(
        f"{host or OLLAMA_HOST}/api/embed",
        data=body.encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            vectors = json.load(resp).get("embeddings")
    except (urllib.error.URLError, OSError, ValueError) as exc:
        raise EmbeddingUnavailable(str(exc)) from exc
    if not isinstance(vectors, list) or len(vectors) != len(texts):
        raise EmbeddingUnavailable(
            f"expected {len(texts)} vectors, got "
            f"{len(vectors) if isinstance(vectors, list) else 'none'}"
        )
    return [normalise(v) for v in vectors]


def normalise(vector: Sequence[float]) -> list[float]:
    norm = math.sqrt(sum(x * x for x in vector))
    if norm == 0:
        # A zero vector is similar to nothing; storing it as such is honest.
        return [0.0] * len(vector)
    return [x / norm for x in vector]


def pack(vector: Sequence[float]) -> bytes:
    return struct.pack(f"<{len(vector)}f", *vector)


def unpack(blob: bytes) -> list[float]:
    return list(struct.unpack(f"<{len(blob) // 4}f", blob))
