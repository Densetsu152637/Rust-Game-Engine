# Editor native hot reload

Editor Play and Reload Game run external Rust as separate native executable generations. Reload Game is an explicit action: it compiles current gameplay sources and prepares the current authored scene snapshot while the previous game keeps running. It does not watch files or execute code on scene edits, file saves, or project open.

The compiler worker owns only the candidate. After compilation it writes `editor-scene.json` into the unique generation directory, gives the candidate a private `editor-user-data` directory there, and waits for the generation startup barrier. At this barrier the executable is alive but gameplay has not run, so it has not opened a game window or acquired save ownership. This confirms process readiness; gameplay configuration still runs after activation and can fail.

The editor's next poll activates the candidate. It requests cooperative shutdown of the previous game and waits for successful teardown before sending the candidate its single start command. The replacement starts from the latest authored scene snapshot. Runtime changes, script objects, ECS state, and runtime saves are not transferred between processes. Source scene files and ordinary player saves remain separate from editor play.

Compilation failure, readiness failure, or Cancel Reload before activation discards the candidate and leaves a still-running previous game alone. Build diagnostics remain available in the editor. A previous game can exit independently during preparation; the editor does not claim it survived in that case. An explicitly requested candidate may still finish preparation and start.

Once old-game teardown begins, rollback is unavailable. A failed or forced teardown prevents candidate activation and ends play with an error. A failed candidate start after teardown also ends play with an error. A game that fails during its own configuration after activation is reported as an exited game on a subsequent poll. None of these outcomes promises restoration of old runtime state.

Stop cancels and joins any compiler/preparation worker, discards an unstarted candidate, then stops the active game. Closing or dropping the host performs the same cleanup. Cancel Reload only discards the pending replacement; it does not stop the active game. Repeated reload requests while a candidate is pending are rejected rather than queued.

The tooling lifecycle tests use real compiled subprocesses to observe the startup barrier, exactly one candidate start, old-game teardown ordering, compile and readiness rejection, cancellation, and stop/drop cleanup. The `editor_hot_reload` acceptance test covers replacement scene snapshots and distinct private save directories with the actual engine entry point. Controller tests preserve authored history on rejection, and a pointer-driven egui test reaches the Reload game and Cancel reload buttons. Neither test establishes in-process state transfer.
