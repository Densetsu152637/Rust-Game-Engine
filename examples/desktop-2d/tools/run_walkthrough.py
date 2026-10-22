"""Run the fixture against an explicitly supplied built engine, preserving logs.
Full mode verifies real project/runtime services across separate processes and
the same per-variant user-data root. Native generation compilation may be cold.
"""
from pathlib import Path
import argparse
import json
import subprocess
import tempfile
from validate_fixture import main as validate_static
ROOT=Path(__file__).resolve().parents[1]

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--engine",type=Path,required=True)
    parser.add_argument("--artifacts",type=Path)
    parser.add_argument("--pure-only",action="store_true")
    parser.add_argument("--timeout",type=float,default=600,help="seconds per native compile/run (default 600)")
    args=parser.parse_args(); engine=args.engine.resolve(); assert engine.is_file()
    artifacts=(args.artifacts or Path(tempfile.mkdtemp(prefix="desktop2d-walkthrough-"))).resolve()
    artifacts.mkdir(parents=True,exist_ok=True)
    validate_static()
    def run(name, arguments, marker=None):
        process=subprocess.run([str(engine),*map(str,arguments)],cwd=engine.parent,encoding="utf-8",capture_output=True,timeout=args.timeout)
        (artifacts/(name+".stdout.json")).write_text(process.stdout,encoding="utf-8")
        (artifacts/(name+".stderr.log")).write_text(process.stderr,encoding="utf-8")
        assert process.returncode==0, name+" failed: "+process.stderr
        result=json.loads(process.stdout); assert result.get("ok") is True, name+" returned ok:false"
        if marker: assert marker in process.stdout+process.stderr, name+" did not reach its assertions"
        print("PASS "+name)
    run("syntax-validate",["project","validate",ROOT/"project-syntax.json"])
    run("pure-validate",["project","validate",ROOT/"project-pure.json"])
    run("pure",["run",ROOT/"project-pure.json","--headless","--ticks",2],"DESKTOP2D_PURE_PASS")
    if not args.pure_only:
        run("interactive-validate",["project","validate",ROOT/"project.json"])
        spec=json.loads((ROOT/"tests/walkthrough.json").read_text())
        for variant in ("male","female"):
            user_data=artifacts/("user-data-"+variant)
            # Reusing an existing directory is intentional for restore, but a fresh
            # artifact directory is required for an independent new walkthrough.
            assert not user_data.exists(), "choose fresh --artifacts for new-game fixture"
            project=ROOT/("project-test-new-"+variant+".json")
            run(variant+"-validate",["project","validate",project])
            command=["run",project,"--headless","--ticks",spec["ticks"],"--user-data",user_data]
            for action in spec["input"]: command.extend(["--input",action])
            run(variant+"-new",command,spec["newMarker"]+variant)
            restore_project=ROOT/("project-test-restore-"+variant+".json")
            run(variant+"-restore",["run",restore_project,"--headless","--ticks",120,"--user-data",user_data],spec["restoreMarker"]+variant)
    print("Evidence: "+str(artifacts))

if __name__=="__main__": main()
