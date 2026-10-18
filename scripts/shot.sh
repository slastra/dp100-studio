#!/bin/bash
# Screenshot DP100 Studio in a headless sway, without touching the session
# you are sitting in.
#
#   scripts/shot.sh OUT.png [WxH] [app args...]
#
# SHOT_NESTED=1 runs sway as a window on your desktop instead (needed for
# clicks; WxH is then the host window's size).
#
# e.g. scripts/shot.sh /tmp/sim.png 600x820 --simulate
#
# The app gets a private runtime, config, state and data directory, so
# nothing it remembers leaks into your real profile. Set SHOT_DELAY (seconds, default 3)
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
mkdir -p "$RT"/{config,state,data} && chmod 700 "$RT"
# SHOT_DATA=DIR seeds the app's data directory (sequences, sessions) from DIR.
[ -n "${SHOT_DATA:-}" ] && cp -r "$SHOT_DATA"/. "$RT/data"
# The app runs under dbus-run-session, so stop it through its parent's pid:
# matching it by name would also hit a DP100 Studio you have open.
trap 'pkill -TERM -P "${APP:-0}" 2>/dev/null; sleep 0.3; kill $APP $SWAY 2>/dev/null; wait 2>/dev/null; rm -rf "$RT"' EXIT

cat > "$RT/sway.conf" <<CONF
default_border none
output HEADLESS-1 resolution $SIZE
CONF
# SHOT_WINDOW=WxH floats the app at that size in the top-left corner and
# captures only it, for a window smaller than the output (nested sway's
# output is the host window's size).
if [ -n "${SHOT_WINDOW:-}" ]; then
    echo "for_window [app_id=\".*\"] floating enable, resize set ${SHOT_WINDOW%x*} ${SHOT_WINDOW#*x}, move position 0 0" >>"$RT/sway.conf"
fi

if [ -n "${SHOT_NESTED:-}" ]; then
    # Nested: sway runs as a window on the host (Hyprland floats it at
    # 1400x900 on workspace 2 without focus; see hyprland.lua). Clicks need
    # this: headless sway has no pointer, and each one-shot wlrctl device
    # vanishes before GTK binds it, so presses are lost.
    HOST=${XDG_RUNTIME_DIR:?}/${WAYLAND_DISPLAY:?SHOT_NESTED needs a host Wayland session}
    env -u DISPLAY XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$HOST" \
        WLR_BACKENDS=wayland WLR_WL_OUTPUTS=1 \
        sway --unsupported-gpu -c "$RT/sway.conf" >"$RT/sway.log" 2>&1 &
else
    env -u WAYLAND_DISPLAY -u DISPLAY XDG_RUNTIME_DIR="$RT" \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        sway --unsupported-gpu -c "$RT/sway.conf" >"$RT/sway.log" 2>&1 &
fi
SWAY=$!
for _ in $(seq 50); do
    SOCK=$(ls "$RT"/wayland-* 2>/dev/null | grep -v lock | head -1)
    [ -n "$SOCK" ] && break
    sleep 0.1
done
[ -n "${SOCK:-}" ] || { echo "sway did not start:" >&2; cat "$RT/sway.log" >&2; exit 1; }
DISP=$(basename "$SOCK")

# A simulated run has its own app id, so it can share your session bus (and
# with it your theme and fonts, through the settings portal). A hardware run
# would collide with a DP100 Studio you have open, so it gets a private bus,
# and stock Adwaita styling with it.
WRAP=
case " $* " in *" --simulate "*) ;; *) WRAP="dbus-run-session --" ;; esac
# SHOT_STOCK=1 forces the private bus anyway, for screenshots in stock
# Adwaita styling (no desktop theme, font or accent) as most users see it.
[ -n "${SHOT_STOCK:-}" ] && WRAP="dbus-run-session --"
env -u DISPLAY XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" GDK_BACKEND=wayland \
    XDG_CONFIG_HOME="$RT/config" XDG_STATE_HOME="$RT/state" XDG_DATA_HOME="$RT/data" \
    $WRAP "$BIN" "$@" >"$RT/app.log" 2>&1 &
APP=$!
sleep "${SHOT_DELAY:-3}"
# wlrctl only moves relative to the cursor, which sway starts in the middle
# of the output. Pin it to the top-left corner first so positions are
# absolute.
if [ -n "${SHOT_POINTER:-}${SHOT_CLICKS:-}" ]; then
    XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wlrctl pointer move -10000 -10000
fi
if [ -n "${SHOT_POINTER:-}" ]; then
    XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wlrctl pointer move $SHOT_POINTER
    sleep 0.5
fi
# SHOT_CLICKS="X Y;X Y;..." clicks at each absolute position in turn.
if [ -n "${SHOT_CLICKS:-}" ]; then
    read -r PX PY <<<"${SHOT_POINTER:-0 0}"
    IFS=';' read -ra CLICKS <<<"$SHOT_CLICKS"
    for c in "${CLICKS[@]}"; do
        read -r X Y <<<"$c"
        XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wlrctl pointer move $((X-PX)) $((Y-PY))
        PX=$X; PY=$Y
        sleep 0.3
        XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wlrctl pointer click left
        sleep "${SHOT_CLICK_DELAY:-1}"
    done
fi
# SHOT_WTYPE="-M ctrl a -m ctrl 3x" passes its words to wtype after the
# clicks (keyboard input scoped to the nested display).
if [ -n "${SHOT_WTYPE:-}" ]; then
    # shellcheck disable=SC2086  # word splitting is the point
    XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" wtype $SHOT_WTYPE
    # SHOT_AFTER: seconds to let the typing take effect before the capture.
    sleep "${SHOT_AFTER:-0.6}"
fi
GEOM=()
[ -n "${SHOT_WINDOW:-}" ] && GEOM=(-g "0,0 $SHOT_WINDOW")
XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$DISP" grim "${GEOM[@]}" "$OUT" || { cat "$RT/app.log" >&2; exit 1; }
grep -v '^$' "$RT/app.log" >&2 || true
echo "$OUT"
