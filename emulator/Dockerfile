# saturnng (HP 48GX / 49G emulator) with its serial "wire" port bridged to TCP.
# Build:  docker build -t hp49g-emu .
# Run:    docker run --rm -d -p 4848:4848 --name calc hp49g-emu   (MODEL=48gx or 48sx for the HP 48)
# Then connect to tcp://localhost:4848 as if it were the calculator's serial port.

FROM debian:trixie-slim AS build
ARG SATURNNG_COMMIT=3f467d2daf0a2c0abc3929a637df77b8e74468aa
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential pkg-config git ca-certificates curl unzip \
      libgtk-4-dev libncurses-dev liblua5.4-dev libglib2.0-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN git clone https://codeberg.org/gwh/saturnng.git . \
    && git checkout ${SATURNNG_COMMIT} \
    && git submodule update --init
# GTK4 is always compiled in (Makefile has no working WITH_GTK=no), so the runtime
# image needs libgtk-4-1 even though only the ncurses TUI is used.
# "make install" is skipped: it also builds the TeX docs.
RUN make -j"$(nproc)" WITH_SDL=no LUA_VERSION=lua5.4
# ROMs from hpcalc.org (same URLs as dist/ROMs/Makefile; -L/-f added). Keep
# curl's default user agent: hpcalc.org serves junk to fake browser agents.
# The size checks catch that and truncated downloads.
RUN mkdir /roms && cd /roms \
    && curl -fsSL https://www.hpcalc.org/hp49/pc/rom/hp4950emurom.zip -o r49.zip && unzip -p r49.zip rom.49g > rom.49g \
    && curl -fsSL https://www.hpcalc.org/hp48/pc/emulators/gxrom-r.zip -o gx.zip && funzip gx.zip > gxrom-r \
    && curl -fsSL https://www.hpcalc.org/hp48/pc/emulators/sxrom-j.zip -o sx.zip && funzip sx.zip > sxrom-j \
    && rm -f *.zip && ls -l \
    && test "$(stat -c %s rom.49g)" = 2097152 \
    && test "$(stat -c %s gxrom-r)" = 524288 \
    && test "$(stat -c %s sxrom-j)" = 262144

FROM debian:trixie-slim
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      libgtk-4-1 libncursesw6 liblua5.4-0 zlib1g socat tmux procps \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/dist/saturn /usr/bin/saturn
COPY --from=build /roms/ /roms/
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY calc-keys.sh /usr/local/bin/calc-keys
COPY calc-screen.sh /usr/local/bin/calc-screen
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/calc-keys /usr/local/bin/calc-screen
ENV MODEL=49g
ENV TERM=xterm-256color
ENV LANG=C.UTF-8
EXPOSE 4848
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
