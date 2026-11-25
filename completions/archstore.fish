# completions for archstore (Rust TUI, v2.x) — flags only, no subcommands
complete -c archstore -f
complete -c archstore -s j -l json -d 'Print results as JSON instead of opening the TUI'
complete -c archstore -l plain -d 'Print results as a plain list instead of opening the TUI'
complete -c archstore -l offline -d 'Use only the local pacman database (no network)'
complete -c archstore -s h -l help -d 'Show help'
complete -c archstore -s V -l version -d 'Show version'
