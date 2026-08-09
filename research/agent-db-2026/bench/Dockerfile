# The benchmark client runs inside Linux because the deployment target is a
# Linux VPS. Measuring embedded engines on macOS would measure APFS and the
# Docker VM's file sharing, not the engine.
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
        numpy==2.* \
        pyarrow \
        "psycopg[binary]" \
        redis \
        duckdb \
        sqlite-vec \
        lancedb \
        qdrant-client

WORKDIR /bench
COPY adapters.py harness.py /bench/
ENV PYTHONUNBUFFERED=1
ENTRYPOINT ["python", "harness.py"]
