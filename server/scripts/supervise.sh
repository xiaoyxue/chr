# Sourced by the launch scripts in this directory. `supervise <name> <command...>` runs the
# command in the background and starts it again whenever it exits, so one crashed process
# never takes the others down. The delay doubles up to 10 s while a process keeps exiting
# within 10 s of starting. `stop_supervised` stops every supervised process.

supervisors=()

supervise() {
  local name=$1
  shift
  (
    child=
    trap '[[ -n $child ]] && kill "$child" 2>/dev/null; wait; exit 0' TERM
    delay=1
    while true; do
      started=$SECONDS
      "$@" &
      child=$!
      status=0
      wait "$child" || status=$?
      child=
      ((SECONDS - started >= 10)) && delay=1
      echo "[$name] exited with status $status; restarting in ${delay}s" >&2
      sleep "$delay" &
      child=$!
      wait "$child" || true
      child=
      ((delay < 10)) && delay=$((delay * 2))
    done
  ) &
  supervisors+=($!)
}

stop_supervised() {
  kill "${supervisors[@]}" 2>/dev/null || true
  wait
}
