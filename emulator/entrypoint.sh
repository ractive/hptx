#!/bin/bash
# Starts saturn (TUI) inside tmux, finds the pty it opened for the wire port,
# and bridges that pty to TCP port 4848 with socat.
set -eu
MODEL=${MODEL:-49g}
TUI=${TUI:-tui}          # tui (1 char/pixel), tui-small (2x2), tui-tiny (braille 2x4)
DATADIR=/data/saturn$MODEL
mkdir -p "$DATADIR"
cd "$DATADIR"
RESET=
case "$MODEL" in
  49g)  [ -e "$DATADIR/rom" ] || cp /roms/rom.49g "$DATADIR/rom" ;;
  48gx) [ -e "$DATADIR/rom" ] || cp /roms/gxrom-r "$DATADIR/rom" ;;
  48sx) [ -e "$DATADIR/rom" ] || cp /roms/sxrom-j "$DATADIR/rom" ;;
  *) echo "unsupported MODEL=$MODEL" >&2; exit 1 ;;
esac
[ -e "$DATADIR/ram" ] || RESET=--reset

# RAM cards, as saturnng's dist/saturn48gx and dist/saturn48sx launchers do:
# 128 KB in port 1, and 4 MB in port 2 for the GX. The SX launcher also makes
# a 128 KB port2, but saturnng compiles port 2 as 4 MB for both models, so
# that file fails to load ("Can't initialize Port 2 from disk"); skip it.
# CARDS=0 runs with empty card slots.
if [ "${CARDS:-1}" = 1 ]; then
  case "$MODEL" in
    48gx|48sx) [ -e port1 ] || dd if=/dev/zero of=port1 bs=1k count=128 2>/dev/null ;;
  esac
  if [ "$MODEL" = 48gx ]; then
    [ -e port2 ] || dd if=/dev/zero of=port2 bs=1k count=4096 2>/dev/null
  fi
fi

# saturn parses argv twice with getopt_long; an unknown "--datadir DIR" in the
# first pass gets its value permuted away, so --datadir=DIR is required.
# --tui needs >= 64 rows + borders; --mono makes pixels real characters so
# tmux capture-pane can dump them.
tmux new-session -d -s calc -x 200 -y 80 \
  "saturn $* $RESET --$MODEL --$TUI --mono --datadir=$DATADIR 2>>/tmp/saturn.log; echo saturn exited \$?; sleep infinity"

# Wait for the emulator and its wire pty (the slave side of the openpty() pair;
# fd 0-2 are the tmux pane tty, the master shows up as /dev/pts/ptmx).
PTY=
for i in $(seq 1 100); do
  PID=$(pgrep -x saturn || true)
  if [ -n "$PID" ]; then
    PANE=$(tmux display -p -t calc '#{pane_tty}')
    PTY=$(for fd in /proc/"$PID"/fd/*; do readlink "$fd"; done 2>/dev/null \
          | grep -E '^/dev/pts/[0-9]+$' | grep -vx "$PANE" | sort -u | head -1 || true)
  fi
  [ -n "$PTY" ] && break
  sleep 0.2
done
if [ -z "$PTY" ]; then
  echo "saturn did not open a wire pty" >&2
  cat /tmp/saturn.log >&2 || true
  tmux capture-pane -p -t calc >&2 || true
  exit 1
fi
echo "$PTY" > /run/calc-pty
echo "saturn ($MODEL) wire port: $PTY"

# Wait until the LCD has content and has not changed for ~1.5 s.
wait_stable() {
  local prev="" cur n=0
  for _ in $(seq 1 150); do
    sleep 0.3
    cur=$(calc-screen)
    if [ "$cur" = "$prev" ] && grep -q '█' <<<"$cur"; then
      n=$((n + 1)); [ $n -ge 5 ] && return 0
    else
      n=0
    fi
    prev=$cur
  done
  echo "warning: screen did not settle" >&2
}

# AUTOSTART=1 (default): answer the first-boot prompts and start the Kermit server.
if [ "${AUTOSTART:-1}" = 1 ]; then
  wait_stable
  if [ -n "$RESET" ]; then
    # First boot shows "Try To Recover Memory?" -> answer NO (softkey F).
    calc-keys f; wait_stable
    # The 49G then shows a "Memory Clear" box with an OK softkey (F);
    # the 48GX and 48SX go straight to the stack.
    if [ "$MODEL" = 49g ]; then calc-keys f; wait_stable; fi
  fi
  # ALPHA ALPHA S E R V E R ENTER
  calc-keys ';' ';' s e r v e r Enter
  wait_stable
  echo "Kermit server started (SERVER)"
fi

# On docker stop: quit saturn via its F7 key so it saves cpu/hdw/ram/mod
# state into $DATADIR, then stop socat.
shutdown() {
  echo "stopping saturn (saving state to $DATADIR)"
  tmux send-keys -t calc F7 || true
  for _ in $(seq 1 50); do pgrep -x saturn >/dev/null || break; sleep 0.1; done
  kill "$SOCAT" 2>/dev/null || true
  exit 0
}
trap shutdown TERM INT

socat -d TCP-LISTEN:4848,reuseaddr,fork "FILE:$PTY,raw,echo=0,b9600,cs8,parenb=0" &
SOCAT=$!
echo "bridged $PTY to tcp:4848"
wait "$SOCAT"
