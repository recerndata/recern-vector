"""The retrieval half of RAG: split documents into chunks, embed and store
them, then fetch the most relevant chunks for a question and build a prompt.

    pip install recern-vector sentence-transformers
    python rag_retrieval.py

Send the printed prompt to the LLM of your choice. The database file is kept
next to this script, so later runs skip indexing.
"""

from pathlib import Path

import recern_vector as rv
from sentence_transformers import SentenceTransformer

DOCUMENTS = {
    "handbook/vacation.md": """
Employees get 25 vacation days per calendar year. Up to 5 unused days can be
carried over to the next year; the rest expire on March 31.
Vacation requests go through the HR portal at least two weeks in advance.
Public holidays do not count against vacation days.
""",
    "handbook/remote.md": """
Everyone can work remotely up to three days a week. Fully remote contracts are
possible after the probation period with a manager's approval.
The company pays for a monitor and a chair once every three years.
""",
    "handbook/expenses.md": """
Travel expenses are reimbursed within 30 days of submitting receipts.
Meals during business trips are covered up to 50 EUR per day.
Taxis are reimbursed only when public transport is not a reasonable option.
""",
}

DB_PATH = Path(__file__).with_name("handbook.rvec")
model = SentenceTransformer("sentence-transformers/all-MiniLM-L6-v2")


def chunks(text: str, size: int = 2) -> list[str]:
    """Groups of `size` sentences. Real projects often split by tokens instead."""
    sentences = [s.strip() + "." for s in " ".join(text.split()).split(".") if s.strip()]
    return [" ".join(sentences[i : i + size]) for i in range(0, len(sentences), size)]


with rv.Database.open_or_create(DB_PATH) as db:
    if "chunks" not in db:
        ids, texts, metadatas = [], [], []
        for source, text in DOCUMENTS.items():
            for n, chunk in enumerate(chunks(text)):
                ids.append(f"{source}#{n}")
                texts.append(chunk)
                metadatas.append({"source": source, "chunk": n, "text": chunk})
        embeddings = model.encode(texts, normalize_embeddings=True)
        collection = db.create_collection("chunks", dim=embeddings.shape[1])
        collection.upsert_many(ids, embeddings, metadatas)
        print(f"indexed {len(ids)} chunks from {len(DOCUMENTS)} documents into {DB_PATH.name}")

    chunks_collection = db["chunks"]
    question = "How many vacation days can I move to next year?"
    hits = chunks_collection.search(model.encode(question, normalize_embeddings=True), k=3)

context = "\n\n".join(f"[{h.metadata['source']}]\n{h.metadata['text']}" for h in hits)
prompt = f"""Answer the question using only the context below. Cite the source.

Context:
{context}

Question: {question}"""
print(prompt)
