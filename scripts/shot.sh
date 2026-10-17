#!/bin/bash
# Screenshot DP100 Studio in a headless sway, without touching the session
# you are sitting in.
#
#   scripts/shot.sh OUT.png [WxH] [app args...]
#
# e.g. scripts/shot.sh /tmp/sim.png 600x820 --simulate
#
# The app gets a private runtime, config and state directory, so nothing it
# remembers leaks into your real profile. Set SHOT_DELAY (seconds, default 3)
# to let the readouts settle before the capture, and SHOT_POINTER="X Y" to
# move a virtual pointer there first (for hover states).
set -u
OUT=${1:?usage: shot.sh OUT.png [WxH] [app args...]}
SIZE=${2:-600x820}
shift $(( $# >= 2 ? 2 : 1 ))
BIN=${DP100_STUDIO_BIN:-./target/debug/dp100-studio}
cd "$(dirname "$0")/.." || exit 1
[ -x "$BIN" ] || { echo "no binary at $BIN; cargo build first" >&2; exit 1; }

# sun_path caps at 108 bytes, so the runtime dir must be short.
RT=/tmp/dps.$$
mkdir -p "$RT"/{config,state} && chmod 700 "$RT"
trap 'kill $APP $SWAY 2>/dev/null; wait 2>/dev/null; rm -rf "$RT"' EXIT

cat > "$RT/sway.conf" <<CONF
default_border none
output HEADLESS-1 resolution $SIZE
CONF

env -u WAYLAND_DISPLAY -u DISPLAY XDG_RUNTIME_DIR="$RT" \
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
    sway --unsupported-gpu -c "$RT/sway.conf" >"$RT/sway.log" 2>&1 &
SWAY=$!
for _ in $(seq 50); do
    SOCK=$(ls "$RT"/wayland-* 2>/dev/null | grep -v lock | head -1)
    [ -n "$SOCK" ] && break
    sleep 0.1
done
[ -n "${SOCK:-}" ] || { echo "sway did not start:" >&2; cat "$RT/sway.log" >&2; exit 1; }
DISP=$(basename "$SOCK")

env -u DISPLAY XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" GDK_BACKEND=wayland \
    XDG_CONFIG_HOME="$RT/config" XDG_STATE_HOME="$RT/state" \
    "$BIN" "$@" >"$RT/app.log" 2>&1 &
APP=$!
sleep "${SHOT_DELAY:-3}"
if [ -n "${SHOT_POINTER:-}" ]; then
    # wlrctl moves relative to where the pointer is; it starts at 0,0.
    XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wlrctl pointer move $SHOT_POINTER
    sleep 0.5
fi
XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" grim "$OUT" || { cat "$RT/app.log" >&2; exit 1; }
grep -v '^$' "$RT/app.log" >&2 || true
echo "$OUT"
