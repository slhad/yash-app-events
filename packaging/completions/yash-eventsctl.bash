_yash_eventsctl() {
  local cur="${COMP_WORDS[COMP_CWORD]}"
  if (( COMP_CWORD == 1 )); then
    COMPREPLY=( $(compgen -W "version status state shutdown profile events capture replay --json --socket --timeout-ms --help --version" -- "$cur") )
  fi
}
complete -F _yash_eventsctl yash-eventsctl
