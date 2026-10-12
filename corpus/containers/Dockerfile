# Wraps the corpus images into virtual-disk containers with the reference tools (qemu-img,
# qemu-io, ewfacquire), so Rescyo's container readers are checked against files those tools
# wrote, not against our own writers (ADR-0002). Reads and writes regular files only.
FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

RUN apt-get update \
 && apt-get install -y --no-install-recommends qemu-utils ewf-tools python3 \
 && rm -rf /var/lib/apt/lists/*

ENV LANG=C.UTF-8 TZ=UTC
COPY wrap_containers.py /usr/local/bin/wrap_containers.py
ENTRYPOINT ["python3", "/usr/local/bin/wrap_containers.py"]
