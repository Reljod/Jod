"""Storage adapters under test.

Every adapter implements the same five operations so the harness can drive them
identically:

    setup()        once, in the parent, before any worker starts
    connect()      once per worker *process*
    append(...)    write one agent event   -> the append-throughput workload
    rmw(key)       read-modify-write +1    -> the correctness / contention workload
    read(run_id)   recent-events query     -> the read-under-write-load workload

Adapters whose name ends in `-naive` are deliberately misused: they use the
plausible-looking primitive rather than the correct one. They exist to show
that the failure being measured belongs to the *usage*, not the engine.
"""

import json
import os
import time

DIM = 384
VEC_TABLE = "memories"


class Unavailable(Exception):
    """Raised at setup when a driver or server is not reachable."""


class Adapter:
    name = "base"
    # what the engine can structurally do, independent of how fast it is
    supports_rmw = True
    supports_vector = False

    def __init__(self, cfg):
        self.cfg = cfg
        self.conn = None

    # -- lifecycle ---------------------------------------------------------
    def setup(self):
        raise NotImplementedError

    def connect(self):
        raise NotImplementedError

    def close(self):
        if self.conn is not None:
            try:
                self.conn.close()
            except Exception:
                pass

    # -- workload ops ------------------------------------------------------
    def append(self, run_id, seq, payload):
        raise NotImplementedError

    def rmw(self, key):
        raise NotImplementedError

    def read(self, run_id, limit=20):
        raise NotImplementedError

    def total_events(self):
        raise NotImplementedError

    def counter_sum(self):
        raise NotImplementedError

    # -- vector ops (optional) --------------------------------------------
    def vec_setup(self):
        raise NotImplementedError

    def vec_add(self, ids, vecs):
        raise NotImplementedError

    def vec_search(self, q, k=10):
        raise NotImplementedError

    def vec_index(self):
        """Build an ANN index if the engine has one. Return a label."""
        return "brute-force"


EVENT_PAYLOAD = json.dumps(
    {
        "type": "tool_call",
        "tool": "Read",
        "input": {"file_path": "/Users/reljod/Developer/Repositories/Projects/Jod/crates/jod-core/src/runner.rs"},
        "cost_usd": 0.0031,
    }
)


# ---------------------------------------------------------------------------
# SQLite
# ---------------------------------------------------------------------------
class SQLiteAdapter(Adapter):
    name = "sqlite"
    supports_vector = True
    #: correct usage: wait for the lock, and take the write lock up front
    busy_timeout_ms = 5000
    begin = "BEGIN IMMEDIATE"

    def _path(self):
        return os.path.join(self.cfg["data_dir"], "jod-sqlite.db")

    def setup(self):
        import sqlite3

        p = self._path()
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(p + suffix)
            except FileNotFoundError:
                pass
        c = sqlite3.connect(p, isolation_level=None)
        c.execute("PRAGMA journal_mode=WAL")
        c.execute("PRAGMA synchronous=NORMAL")
        c.execute(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, run_id TEXT, seq INTEGER,"
            " ts REAL, payload TEXT)"
        )
        c.execute("CREATE INDEX ix_events_run ON events(run_id, seq)")
        c.execute("CREATE TABLE counters (k TEXT PRIMARY KEY, n INTEGER)")
        for i in range(self.cfg["n_keys"]):
            c.execute("INSERT INTO counters VALUES (?, 0)", (f"task-{i}",))
        c.close()

    def connect(self):
        import sqlite3

        self.conn = sqlite3.connect(self._path(), isolation_level=None, timeout=self.busy_timeout_ms / 1000)
        self.conn.execute(f"PRAGMA busy_timeout={self.busy_timeout_ms}")
        self.conn.execute("PRAGMA synchronous=NORMAL")

    def append(self, run_id, seq, payload):
        self.conn.execute(
            "INSERT INTO events (run_id, seq, ts, payload) VALUES (?,?,?,?)",
            (run_id, seq, time.time(), payload),
        )

    def rmw(self, key):
        c = self.conn
        c.execute(self.begin)
        try:
            n = c.execute("SELECT n FROM counters WHERE k=?", (key,)).fetchone()[0]
            c.execute("UPDATE counters SET n=? WHERE k=?", (n + 1, key))
            c.execute("COMMIT")
        except Exception:
            c.execute("ROLLBACK")
            raise

    def read(self, run_id, limit=20):
        return len(
            self.conn.execute(
                "SELECT payload FROM events WHERE run_id=? ORDER BY seq DESC LIMIT ?",
                (run_id, limit),
            ).fetchall()
        )

    def total_events(self):
        return self.conn.execute("SELECT count(*) FROM events").fetchone()[0]

    def counter_sum(self):
        return self.conn.execute("SELECT sum(n) FROM counters").fetchone()[0]

    # -- vector --
    def _vec_conn(self):
        import sqlite3

        import sqlite_vec

        c = sqlite3.connect(os.path.join(self.cfg["data_dir"], "jod-vec.db"), isolation_level=None)
        c.enable_load_extension(True)
        sqlite_vec.load(c)
        c.enable_load_extension(False)
        return c

    def vec_setup(self):
        p = os.path.join(self.cfg["data_dir"], "jod-vec.db")
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(p + suffix)
            except FileNotFoundError:
                pass
        self.vconn = self._vec_conn()
        self.vconn.execute("PRAGMA journal_mode=WAL")
        self.vconn.execute(
            f"CREATE VIRTUAL TABLE {VEC_TABLE} USING vec0(id INTEGER PRIMARY KEY, embedding float[{DIM}])"
        )

    def vec_add(self, ids, vecs):
        import sqlite_vec

        self.vconn.execute("BEGIN")
        self.vconn.executemany(
            f"INSERT INTO {VEC_TABLE} (id, embedding) VALUES (?,?)",
            [(int(i), sqlite_vec.serialize_float32(v.tolist())) for i, v in zip(ids, vecs)],
        )
        self.vconn.execute("COMMIT")

    def vec_search(self, q, k=10):
        import sqlite_vec

        rows = self.vconn.execute(
            f"SELECT id FROM {VEC_TABLE} WHERE embedding MATCH ? AND k = ? ORDER BY distance",
            (sqlite_vec.serialize_float32(q.tolist()), k),
        ).fetchall()
        return [r[0] for r in rows]


class SQLiteNaiveAdapter(SQLiteAdapter):
    """The same engine, configured the way people configure it by accident."""

    name = "sqlite-naive"
    busy_timeout_ms = 0
    begin = "BEGIN"  # deferred: the write lock is taken late, so upgrades collide


# ---------------------------------------------------------------------------
# PostgreSQL
# ---------------------------------------------------------------------------
class PostgresAdapter(Adapter):
    name = "postgres"
    supports_vector = True
    lock_clause = " FOR UPDATE"

    def _dsn(self):
        return self.cfg["pg_dsn"]

    def setup(self):
        import psycopg

        with psycopg.connect(self._dsn(), autocommit=True) as c:
            c.execute("DROP TABLE IF EXISTS events")
            c.execute("DROP TABLE IF EXISTS counters")
            c.execute(
                "CREATE TABLE events (id bigserial PRIMARY KEY, run_id text, seq int,"
                " ts double precision, payload jsonb)"
            )
            c.execute("CREATE INDEX ix_events_run ON events(run_id, seq)")
            c.execute("CREATE TABLE counters (k text PRIMARY KEY, n int)")
            for i in range(self.cfg["n_keys"]):
                c.execute("INSERT INTO counters VALUES (%s, 0)", (f"task-{i}",))

    def connect(self):
        import psycopg

        self.conn = psycopg.connect(self._dsn(), autocommit=True)
        self.cur = self.conn.cursor()

    def append(self, run_id, seq, payload):
        self.cur.execute(
            "INSERT INTO events (run_id, seq, ts, payload) VALUES (%s,%s,%s,%s)",
            (run_id, seq, time.time(), payload),
        )

    def rmw(self, key):
        c = self.conn
        c.execute("BEGIN")
        try:
            self.cur.execute(f"SELECT n FROM counters WHERE k=%s{self.lock_clause}", (key,))
            n = self.cur.fetchone()[0]
            self.cur.execute("UPDATE counters SET n=%s WHERE k=%s", (n + 1, key))
            c.execute("COMMIT")
        except Exception:
            c.execute("ROLLBACK")
            raise

    def read(self, run_id, limit=20):
        self.cur.execute(
            "SELECT payload FROM events WHERE run_id=%s ORDER BY seq DESC LIMIT %s",
            (run_id, limit),
        )
        return len(self.cur.fetchall())

    def total_events(self):
        self.cur.execute("SELECT count(*) FROM events")
        return self.cur.fetchone()[0]

    def counter_sum(self):
        self.cur.execute("SELECT sum(n) FROM counters")
        return int(self.cur.fetchone()[0])

    # -- vector --
    def vec_setup(self):
        import psycopg

        self.vconn = psycopg.connect(self._dsn(), autocommit=True)
        self.vcur = self.vconn.cursor()
        self.vcur.execute("CREATE EXTENSION IF NOT EXISTS vector")
        self.vcur.execute(f"DROP TABLE IF EXISTS {VEC_TABLE}")
        self.vcur.execute(f"CREATE TABLE {VEC_TABLE} (id bigint PRIMARY KEY, embedding vector({DIM}))")

    def vec_add(self, ids, vecs):
        with self.vcur.copy(f"COPY {VEC_TABLE} (id, embedding) FROM STDIN") as cp:
            for i, v in zip(ids, vecs):
                cp.write_row((int(i), "[" + ",".join(f"{x:.6f}" for x in v.tolist()) + "]"))

    def vec_index(self):
        # PG_EF_SEARCH=0 skips the index entirely, which is the control case:
        # it must return 100% recall, proving the data and the query are right
        # and that any shortfall belongs to the index, not the harness.
        ef = int(os.environ.get("PG_EF_SEARCH", "40"))
        m = int(os.environ.get("PG_HNSW_M", "16"))
        efc = int(os.environ.get("PG_HNSW_EFC", "64"))
        if ef == 0:
            return "seqscan (no index, exact)"
        self.vcur.execute(
            f"CREATE INDEX ON {VEC_TABLE} USING hnsw (embedding vector_cosine_ops)"
            f" WITH (m={m}, ef_construction={efc})"
        )
        self.vcur.execute(f"SET hnsw.ef_search = {ef}")
        self.vcur.execute(
            f"EXPLAIN SELECT id FROM {VEC_TABLE} ORDER BY embedding <=> "
            "'[" + ",".join(["0.1"] * DIM) + "]'::vector LIMIT 10"
        )
        plan = " ".join(r[0] for r in self.vcur.fetchall())
        used = "Index Scan" in plan
        return f"hnsw(m={m},ef_c={efc},ef_s={ef}){'' if used else ' [INDEX NOT USED]'}"

    def vec_search(self, q, k=10):
        vs = "[" + ",".join(f"{x:.6f}" for x in q.tolist()) + "]"
        self.vcur.execute(
            f"SELECT id FROM {VEC_TABLE} ORDER BY embedding <=> %s::vector LIMIT %s", (vs, k)
        )
        return [r[0] for r in self.vcur.fetchall()]


class PostgresNaiveAdapter(PostgresAdapter):
    """READ COMMITTED without FOR UPDATE — the classic lost-update recipe."""

    name = "postgres-naive"
    lock_clause = ""


# ---------------------------------------------------------------------------
# Redis 8
# ---------------------------------------------------------------------------
class RedisAdapter(Adapter):
    name = "redis"
    supports_vector = True

    def _client(self):
        import redis

        return redis.Redis(host=self.cfg["redis_host"], port=6379, decode_responses=True)

    def setup(self):
        c = self._client()
        c.flushall()
        for i in range(self.cfg["n_keys"]):
            c.set(f"task-{i}", 0)

    def connect(self):
        self.conn = self._client()

    def append(self, run_id, seq, payload):
        self.conn.xadd(f"stream:{run_id}", {"seq": seq, "ts": time.time(), "payload": payload})

    def rmw(self, key):
        self.conn.incr(key)  # atomic by construction

    def read(self, run_id, limit=20):
        return len(self.conn.xrevrange(f"stream:{run_id}", count=limit))

    def total_events(self):
        return sum(self.conn.xlen(k) for k in self.conn.scan_iter("stream:*"))

    def counter_sum(self):
        return sum(int(self.conn.get(f"task-{i}") or 0) for i in range(self.cfg["n_keys"]))

    # -- vector (Redis 8 vector sets) --
    def vec_setup(self):
        self.vconn = self._client()
        self.vconn.delete(VEC_TABLE)

    def vec_add(self, ids, vecs):
        pipe = self.vconn.pipeline(transaction=False)
        for i, v in zip(ids, vecs):
            pipe.execute_command("VADD", VEC_TABLE, "VALUES", DIM, *[repr(float(x)) for x in v.tolist()], str(int(i)))
        pipe.execute()

    def vec_index(self):
        return "hnsw (vector set, default)"

    def vec_search(self, q, k=10):
        res = self.vconn.execute_command(
            "VSIM", VEC_TABLE, "VALUES", DIM, *[repr(float(x)) for x in q.tolist()], "COUNT", k
        )
        return [int(x) for x in res]


class RedisNaiveAdapter(RedisAdapter):
    """GET then SET — fast, obvious, and wrong."""

    name = "redis-naive"

    def rmw(self, key):
        n = int(self.conn.get(key) or 0)
        self.conn.set(key, n + 1)


# ---------------------------------------------------------------------------
# LanceDB
# ---------------------------------------------------------------------------
class LanceDBAdapter(Adapter):
    name = "lancedb"
    supports_vector = True
    supports_rmw = True  # it has an update() call; whether it is safe is the question

    def _uri(self):
        return os.path.join(self.cfg["data_dir"], "lancedb")

    def setup(self):
        import shutil

        import lancedb
        import pyarrow as pa

        shutil.rmtree(self._uri(), ignore_errors=True)
        db = lancedb.connect(self._uri())
        db.create_table(
            "events",
            schema=pa.schema(
                [
                    pa.field("run_id", pa.string()),
                    pa.field("seq", pa.int64()),
                    pa.field("ts", pa.float64()),
                    pa.field("payload", pa.string()),
                ]
            ),
        )
        db.create_table(
            "counters",
            data=[{"k": f"task-{i}", "n": 0} for i in range(self.cfg["n_keys"])],
        )

    def connect(self):
        import lancedb

        self.conn = lancedb.connect(self._uri())
        self.events = self.conn.open_table("events")
        self.counters = self.conn.open_table("counters")

    def append(self, run_id, seq, payload):
        self.events.add([{"run_id": run_id, "seq": seq, "ts": time.time(), "payload": payload}])

    def rmw(self, key):
        rows = self.counters.search().where(f"k = '{key}'").limit(1).to_list()
        n = rows[0]["n"]
        self.counters.update(where=f"k = '{key}'", values={"n": n + 1})

    def read(self, run_id, limit=20):
        return len(self.events.search().where(f"run_id = '{run_id}'").limit(limit).to_list())

    def total_events(self):
        return self.events.count_rows()

    def counter_sum(self):
        return sum(r["n"] for r in self.counters.search().limit(10_000).to_list())

    # -- vector --
    def vec_setup(self):
        import shutil

        import lancedb
        import pyarrow as pa

        uri = os.path.join(self.cfg["data_dir"], "lancedb-vec")
        shutil.rmtree(uri, ignore_errors=True)
        self.vdb = lancedb.connect(uri)
        self.vtable = self.vdb.create_table(
            VEC_TABLE,
            schema=pa.schema(
                [pa.field("id", pa.int64()), pa.field("vector", pa.list_(pa.float32(), DIM))]
            ),
        )

    def vec_add(self, ids, vecs):
        self.vtable.add([{"id": int(i), "vector": v.tolist()} for i, v in zip(ids, vecs)])

    def vec_index(self):
        try:
            self.vtable.create_index(metric="cosine", num_partitions=64, num_sub_vectors=48)
            return "ivf_pq(64,48)"
        except Exception as e:  # noqa: BLE001 - index is optional, brute force still answers
            return f"brute-force ({type(e).__name__})"

    def vec_search(self, q, k=10):
        return [r["id"] for r in self.vtable.search(q.tolist()).metric("cosine").limit(k).to_list()]


# ---------------------------------------------------------------------------
# Qdrant
# ---------------------------------------------------------------------------
class QdrantAdapter(Adapter):
    name = "qdrant"
    supports_vector = True

    def _client(self):
        from qdrant_client import QdrantClient

        return QdrantClient(host=self.cfg["qdrant_host"], port=6333, timeout=30)

    def setup(self):
        from qdrant_client.models import Distance, VectorParams

        c = self._client()
        for coll in ("events", "counters"):
            try:
                c.delete_collection(coll)
            except Exception:
                pass
            c.create_collection(coll, vectors_config=VectorParams(size=4, distance=Distance.COSINE))
        from qdrant_client.models import PointStruct

        c.upsert(
            "counters",
            points=[
                PointStruct(id=i, vector=[0.0] * 4, payload={"k": f"task-{i}", "n": 0})
                for i in range(self.cfg["n_keys"])
            ],
        )
        self._counter_ids = {f"task-{i}": i for i in range(self.cfg["n_keys"])}

    def connect(self):
        self.conn = self._client()
        self._counter_ids = {f"task-{i}": i for i in range(self.cfg["n_keys"])}
        self._seq = 0

    def append(self, run_id, seq, payload):
        from qdrant_client.models import PointStruct

        pid = abs(hash((run_id, seq, os.getpid()))) % (2**60)
        self.conn.upsert(
            "events",
            points=[
                PointStruct(
                    id=pid,
                    vector=[0.0] * 4,
                    payload={"run_id": run_id, "seq": seq, "ts": time.time(), "payload": payload},
                )
            ],
            wait=True,
        )

    def rmw(self, key):
        pid = self._counter_ids[key]
        pts = self.conn.retrieve("counters", ids=[pid], with_payload=True)
        n = pts[0].payload["n"]
        self.conn.set_payload("counters", payload={"n": n + 1}, points=[pid], wait=True)

    def read(self, run_id, limit=20):
        from qdrant_client.models import FieldCondition, Filter, MatchValue

        res = self.conn.scroll(
            "events",
            scroll_filter=Filter(must=[FieldCondition(key="run_id", match=MatchValue(value=run_id))]),
            limit=limit,
        )
        return len(res[0])

    def total_events(self):
        return self.conn.count("events", exact=True).count

    def counter_sum(self):
        pts = self.conn.retrieve("counters", ids=list(self._counter_ids.values()), with_payload=True)
        return sum(p.payload["n"] for p in pts)

    # -- vector --
    def vec_setup(self):
        from qdrant_client.models import Distance, VectorParams

        self.vconn = self._client()
        try:
            self.vconn.delete_collection(VEC_TABLE)
        except Exception:
            pass
        self.vconn.create_collection(
            VEC_TABLE, vectors_config=VectorParams(size=DIM, distance=Distance.COSINE)
        )

    def vec_add(self, ids, vecs):
        from qdrant_client.models import PointStruct

        self.vconn.upsert(
            VEC_TABLE,
            points=[PointStruct(id=int(i), vector=v.tolist()) for i, v in zip(ids, vecs)],
            wait=True,
        )

    def vec_index(self):
        return "hnsw(m=16, default)"

    def vec_search(self, q, k=10):
        res = self.vconn.query_points(VEC_TABLE, query=q.tolist(), limit=k).points
        return [p.id for p in res]


# ---------------------------------------------------------------------------
# DuckDB — included to measure the shape of its failure, not its speed
# ---------------------------------------------------------------------------
class DuckDBAdapter(Adapter):
    name = "duckdb"

    def _path(self):
        return os.path.join(self.cfg["data_dir"], "jod-duck.db")

    def setup(self):
        import duckdb

        try:
            os.remove(self._path())
        except FileNotFoundError:
            pass
        c = duckdb.connect(self._path())
        c.execute("CREATE SEQUENCE ev_id START 1")
        c.execute(
            "CREATE TABLE events (id BIGINT DEFAULT nextval('ev_id'), run_id VARCHAR,"
            " seq INTEGER, ts DOUBLE, payload VARCHAR)"
        )
        c.execute("CREATE TABLE counters (k VARCHAR PRIMARY KEY, n INTEGER)")
        for i in range(self.cfg["n_keys"]):
            c.execute("INSERT INTO counters VALUES (?, 0)", (f"task-{i}",))
        c.close()

    def connect(self):
        import duckdb

        self.conn = duckdb.connect(self._path())

    def append(self, run_id, seq, payload):
        self.conn.execute(
            "INSERT INTO events (run_id, seq, ts, payload) VALUES (?,?,?,?)",
            (run_id, seq, time.time(), payload),
        )

    def rmw(self, key):
        n = self.conn.execute("SELECT n FROM counters WHERE k=?", (key,)).fetchone()[0]
        self.conn.execute("UPDATE counters SET n=? WHERE k=?", (n + 1, key))

    def read(self, run_id, limit=20):
        return len(
            self.conn.execute(
                "SELECT payload FROM events WHERE run_id=? ORDER BY seq DESC LIMIT ?",
                (run_id, limit),
            ).fetchall()
        )

    def total_events(self):
        return self.conn.execute("SELECT count(*) FROM events").fetchone()[0]

    def counter_sum(self):
        return self.conn.execute("SELECT sum(n) FROM counters").fetchone()[0]


REGISTRY = {
    a.name: a
    for a in [
        SQLiteAdapter,
        SQLiteNaiveAdapter,
        PostgresAdapter,
        PostgresNaiveAdapter,
        RedisAdapter,
        RedisNaiveAdapter,
        LanceDBAdapter,
        QdrantAdapter,
        DuckDBAdapter,
    ]
}
