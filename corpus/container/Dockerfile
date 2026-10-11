# Builds Rescyo's test corpus with the reference file-system tools. Runs only inside this
# container (ADR-0002): nothing here ever sees a block device of the host.
FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      dosfstools mtools exfatprogs exfat-fuse ntfs-3g fdisk fuse3 python3 locales \
      wimtools attr e2fsprogs \
 && sed -i 's/^# *\(en_US.UTF-8\)/\1/' /etc/locale.gen && locale-gen \
 && rm -rf /var/lib/apt/lists/*

ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 TZ=UTC MTOOLS_SKIP_CHECK=1
COPY build_corpus.py /usr/local/bin/build_corpus.py
ENTRYPOINT ["python3", "/usr/local/bin/build_corpus.py"]
