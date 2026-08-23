#!/bin/bash
# Guest-side driver. Always the isolated Xvfb — never a host DISPLAY.
set -euo pipefail

export DISPLAY=:99

SCROLL_MAX=40

usage() {
  cat <<'EOF' >&2
Usage: action.sh <command> [args]
  screenshot              write a PNG of the guest DISPLAY to stdout
  click X Y [BUTTON]      BUTTON: left|right|middle (default left)
  type TEXT               type TEXT into the guest
  key KEY [KEY...]        key chord in the guest
  scroll X Y DY [DX]
  move X Y
  wait MS
EOF
}

map_key() {
  local k="${1// /}"
  case "$k" in
    META|Meta|meta|CMD|Cmd|cmd|SUPER|Super|super|WIN|Win|win|WINDOWS|Windows)
      echo super
      ;;
    CTRL|Ctrl|ctrl|CONTROL|Control|control)
      echo ctrl
      ;;
    ALT|Alt|alt)
      echo alt
      ;;
    SHIFT|Shift|shift)
      echo shift
      ;;
    ENTER|Enter|enter|RETURN|Return|return)
      echo Return
      ;;
    ESC|Esc|esc|ESCAPE|Escape|escape)
      echo Escape
      ;;
    SPACE|Space|space)
      echo space
      ;;
    TAB|Tab|tab)
      echo Tab
      ;;
    BACKSPACE|Backspace|backspace)
      echo BackSpace
      ;;
    DELETE|Delete|delete|DEL|Del)
      echo Delete
      ;;
    ARROW_UP|ArrowUp|ARROWUP|UP|Up|up)
      echo Up
      ;;
    ARROW_DOWN|ArrowDown|ARROWDOWN|DOWN|Down|down)
      echo Down
      ;;
    ARROW_LEFT|ArrowLeft|ARROWLEFT|LEFT|Left|left)
      echo Left
      ;;
    ARROW_RIGHT|ArrowRight|ARROWRIGHT|RIGHT|Right)
      echo Right
      ;;
    PAGE_UP|PageUp|PAGEUP|Page_Up)
      echo Prior
      ;;
    PAGE_DOWN|PageDown|PAGEDOWN|Page_Down)
      echo Next
      ;;
    HOME|Home)
      echo Home
      ;;
    END|End)
      echo End
      ;;
    [fF][0-9]|[fF]1[0-2])
      printf 'F%s\n' "${k#[fF]}"
      ;;
    *)
      echo "$k"
      ;;
  esac
}

is_int() {
  [[ "${1:-}" =~ ^-?[0-9]+$ ]]
}

clamp_ticks() {
  local n="$1"
  if [ "$n" -gt "$SCROLL_MAX" ]; then
    echo "$SCROLL_MAX"
  elif [ "$n" -lt "$((-SCROLL_MAX))" ]; then
    echo "$((-SCROLL_MAX))"
  else
    echo "$n"
  fi
}

require_display() {
  if ! xdpyinfo >/dev/null 2>&1; then
    echo "action.sh: guest DISPLAY=${DISPLAY} is not available" >&2
    exit 1
  fi
}

cmd_screenshot() {
  _berthos_shot="$(mktemp /tmp/berthos-shot.XXXXXX.png)"
  trap 'rm -f -- "${_berthos_shot:-}"' EXIT
  trap 'rm -f -- "${_berthos_shot:-}"; exit 141' PIPE
  if ! import -display "$DISPLAY" -silent -window root "png:${_berthos_shot}"; then
    echo "action.sh: screenshot failed" >&2
    exit 1
  fi
  cat "$_berthos_shot"
}

cmd_click() {
  local x="${1:-}" y="${2:-}" button="${3:-left}" b repeat=1
  if ! is_int "$x" || ! is_int "$y"; then
    echo "action.sh: click requires X Y" >&2
    exit 1
  fi
  case "$button" in
    left|1) b=1 ;;
    middle|2) b=2 ;;
    right|3) b=3 ;;
    double|double_left) b=1; repeat=2 ;;
    double_middle) b=2; repeat=2 ;;
    double_right) b=3; repeat=2 ;;
    *)
      echo "action.sh: unknown button '$button'" >&2
      exit 1
      ;;
  esac
  xdotool mousemove --sync "$x" "$y"
  xdotool click --clearmodifiers --repeat "$repeat" --delay 50 "$b"
}

cmd_type() {
  local text
  if [ "$#" -eq 0 ]; then
    echo "action.sh: type requires TEXT" >&2
    exit 1
  else
    text="$*"
  fi
  if [ -z "$text" ]; then
    return 0
  fi
  local chunk
  while [ -n "$text" ]; do
    chunk="${text:0:64}"
    text="${text:64}"
    xdotool type --clearmodifiers --delay 12 -- "$chunk"
  done
}

cmd_key() {
  if [ "$#" -eq 0 ]; then
    echo "action.sh: key requires KEY" >&2
    exit 1
  fi
  local joined parts mapped k
  joined="$(printf '%s+' "$@")"
  joined="${joined%+}"
  IFS='+' read -ra parts <<<"$joined"
  mapped=()
  for k in "${parts[@]}"; do
    [ -z "$k" ] && continue
    mapped+=("$(map_key "$k")")
  done
  if [ "${#mapped[@]}" -eq 0 ]; then
    echo "action.sh: key requires KEY" >&2
    exit 1
  fi
  local chord
  chord="$(IFS=+; echo "${mapped[*]}")"
  xdotool key --clearmodifiers -- "$chord"
}

cmd_scroll() {
  local x="${1:-}" y="${2:-}" dx dy
  if ! is_int "$x" || ! is_int "$y"; then
    echo "action.sh: scroll requires X Y DY or X Y DX DY" >&2
    exit 1
  fi
  if [ "$#" -eq 3 ]; then
    dx=0
    dy="$3"
  elif [ "$#" -eq 4 ]; then
    dx="$3"
    dy="$4"
  else
    echo "action.sh: scroll requires X Y DY or X Y DX DY" >&2
    exit 1
  fi
  if ! is_int "$dx" || ! is_int "$dy"; then
    echo "action.sh: scroll ticks must be integers" >&2
    exit 1
  fi
  dx="$(clamp_ticks "$dx")"
  dy="$(clamp_ticks "$dy")"
  xdotool mousemove --sync "$x" "$y"
  if [ "$dy" -gt 0 ]; then
    xdotool click --clearmodifiers --repeat "$dy" --delay 20 5
  elif [ "$dy" -lt 0 ]; then
    xdotool click --clearmodifiers --repeat "$((-dy))" --delay 20 4
  fi
  if [ "$dx" -gt 0 ]; then
    xdotool click --clearmodifiers --repeat "$dx" --delay 20 7
  elif [ "$dx" -lt 0 ]; then
    xdotool click --clearmodifiers --repeat "$((-dx))" --delay 20 6
  fi
}

cmd_move() {
  local x="${1:-}" y="${2:-}"
  if ! is_int "$x" || ! is_int "$y"; then
    echo "action.sh: move requires X Y" >&2
    exit 1
  fi
  xdotool mousemove --sync "$x" "$y"
}

cmd_wait() {
  local ms="${1:-}"
  if ! [[ "${ms}" =~ ^[0-9]+$ ]]; then
    echo "action.sh: wait requires MS milliseconds" >&2
    exit 1
  fi
  sleep "$(awk "BEGIN { printf \"%.3f\", $ms/1000 }")"
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] || [ $# -lt 1 ]; then
  usage
  if [ $# -lt 1 ]; then
    exit 2
  fi
  exit 0
fi

op="$1"
shift

case "$op" in
  screenshot|click|type|key|scroll|move|wait) ;;
  *)
    echo "action.sh: unknown command '$op'" >&2
    usage
    exit 2
    ;;
esac

if [ "$op" != "wait" ]; then
  require_display
fi

case "$op" in
  screenshot) cmd_screenshot ;;
  click) cmd_click "$@" ;;
  type) cmd_type "$@" ;;
  key) cmd_key "$@" ;;
  scroll) cmd_scroll "$@" ;;
  move) cmd_move "$@" ;;
  wait) cmd_wait "$@" ;;
esac
