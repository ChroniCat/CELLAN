#!/usr/bin/env python3
"""实施进度清单维护工具。

docs/excel-wps-audit-2026-11-12/tasks.json 是只读的需求清单；
implementation-progress.json 是本工具维护的可恢复进度记录，覆盖全部 61 项任务和 148 个切片。

用法：
  python scripts/audit_progress.py init
  python scripts/audit_progress.py set --id UI-01 --acceptance "..." --file crates/app/src/main.rs \
      --command "cargo test ..." --evidence implementation-evidence/ui01-*.txt --status 待验收 --next "..."
  python scripts/audit_progress.py slice --id UI-02 --slice UI-02.1 --status 实施中
  python scripts/audit_progress.py show --id UI-01
  python scripts/audit_progress.py counts
  python scripts/audit_progress.py check

列表字段（--file/--commit/--command/--evidence/--not-done/--failure）可重复；
--replace-<字段> 会先清空该字段再写入。空字符串值表示清空。
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AUDIT = os.path.join(ROOT, "docs", "excel-wps-audit-2026-11-12")
TASKS = os.path.join(AUDIT, "tasks.json")
PROGRESS = os.path.join(AUDIT, "implementation-progress.json")

STATUSES = ["待实施", "实施中", "待验收", "阻塞", "已验收"]
LIST_FIELDS = ["files", "commits", "commands", "evidence", "not_done", "failures"]


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_tasks():
    with open(TASKS, encoding="utf-8") as f:
        return json.load(f)


def load_progress():
    with open(PROGRESS, encoding="utf-8") as f:
        return json.load(f)


def save_progress(p):
    p["updated_utc"] = now()
    tmp = PROGRESS + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        json.dump(p, f, ensure_ascii=False, indent=1)
        f.write("\n")
    os.replace(tmp, PROGRESS)


def blank(status="待实施"):
    d = {
        "status": status,
        "acceptance": "",
        "files": [],
        "commits": [],
        "commands": [],
        "evidence": [],
        "not_done": [],
        "failures": [],
        "blocker": "",
        "next": "",
        "updated_utc": now(),
    }
    return d


def cmd_init(args):
    src = load_tasks()
    if os.path.exists(PROGRESS) and not args.force:
        print(f"已存在：{PROGRESS}（用 --force 重新生成，会丢弃现有进度）")
        return 1
    p = {
        "schema": 1,
        "source": "docs/excel-wps-audit-2026-11-12/tasks.json",
        "source_baseline": src.get("baseline"),
        "source_date": src.get("date"),
        "branch": args.branch,
        "note": "状态只能是：待实施/实施中/待验收/阻塞/已验收。已实现未验证只能标待验收。父任务全部切片+整体验收通过后才能标已验收。",
        "updated_utc": now(),
        "tasks": [],
    }
    for t in src["tasks"]:
        entry = {
            "id": t["id"],
            "title": t["title"],
            "priority": t["priority"],
            "depends_on": t.get("depends_on", []),
            "size": t.get("size", "S"),
            "scope": t.get("scope", ""),
            "source": t.get("source", ""),
            "heading": t.get("heading", ""),
            **blank(),
        }
        for s in t.get("slices", []):
            entry.setdefault("slices", []).append(
                {
                    "id": s["id"],
                    "title": s["title"],
                    "priority": s.get("priority", t["priority"]),
                    "acceptance_source": s.get("acceptance_source", ""),
                    "depends_on": s.get("depends_on", []),
                    **blank(),
                }
            )
        p["tasks"].append(entry)
    save_progress(p)
    print(f"已生成 {PROGRESS}：任务 {len(p['tasks'])}，切片 {sum(len(t.get('slices', [])) for t in p['tasks'])}")
    return 0


def find(p, task_id, slice_id=None):
    for t in p["tasks"]:
        if t["id"] == task_id:
            if slice_id is None:
                return t, None
            for s in t.get("slices", []):
                if s["id"] == slice_id:
                    return t, s
            raise SystemExit(f"找不到切片 {slice_id}（任务 {task_id}）")
    raise SystemExit(f"找不任务 {task_id}")


def apply(node, args):
    if args.status:
        if args.status not in STATUSES:
            raise SystemExit(f"非法状态 {args.status}；允许：{STATUSES}")
        node["status"] = args.status
    if args.acceptance is not None:
        node["acceptance"] = args.acceptance
    if args.next is not None:
        node["next"] = args.next
    if args.blocker is not None:
        node["blocker"] = args.blocker
    for f in LIST_FIELDS:
        replace = getattr(args, "replace_" + f)
        values = getattr(args, f) or []
        if replace:
            node[f] = list(values)
        else:
            for v in values:
                if v not in node[f]:
                    node[f].append(v)
    node["updated_utc"] = now()


def cmd_set(args, slice_mode=False):
    p = load_progress()
    task, _ = find(p, args.id, None)
    node = task
    if slice_mode:
        found = find(p, args.id, args.slice)
        if found[1] is None:
            raise SystemExit(f"任务 {args.id} 没有切片 {args.slice}")
        node = found[1]
    apply(node, args)
    save_progress(p)
    print(f"{node['id']}: {node['status']}")
    return 0


def cmd_show(args):
    p = load_progress()
    t, _ = find(p, args.id)
    print(json.dumps(t, ensure_ascii=False, indent=1))
    return 0


def counts(p):
    tasks = p["tasks"]
    slices = [s for t in tasks for s in t.get("slices", [])]
    def c(items, st):
        return sum(1 for x in items if x["status"] == st)
    return {
        "tasks_total": len(tasks),
        "tasks": {st: c(tasks, st) for st in STATUSES},
        "slices_total": len(slices),
        "slices": {st: c(slices, st) for st in STATUSES},
    }


def cmd_counts(args):
    p = load_progress()
    k = counts(p)
    print(json.dumps(k, ensure_ascii=False, indent=1))
    return 0


def cmd_check(args):
    p = load_progress()
    problems = []
    for t in p["tasks"]:
        if t["status"] not in STATUSES:
            problems.append(f"{t['id']}: 非法状态 {t['status']}")
        if t["status"] == "已验收":
            if not t["acceptance"]:
                problems.append(f"{t['id']}: 已验收缺少 acceptance")
            if not t["commands"]:
                problems.append(f"{t['id']}: 已验收缺少 commands")
            if not t["evidence"]:
                problems.append(f"{t['id']}: 已验收缺少 evidence")
            if t["blocker"]:
                problems.append(f"{t['id']}: 已验收但仍有阻塞：{t['blocker']}")
            for s in t.get("slices", []):
                if s["status"] != "已验收":
                    problems.append(f"{t['id']}: 父任务已验收但切片 {s['id']} 是 {s['status']}")
        for s in t.get("slices", []):
            if s["status"] not in STATUSES:
                problems.append(f"{s['id']}: 非法状态 {s['status']}")
            if s["status"] == "已验收":
                if not s["acceptance"]:
                    problems.append(f"{s['id']}: 已验收缺少 acceptance")
                if not s["commands"]:
                    problems.append(f"{s['id']}: 已验收缺少 commands")
                if not s["evidence"]:
                    problems.append(f"{s['id']}: 已验收缺少 evidence")
                if s["blocker"]:
                    problems.append(f"{s['id']}: 已验收但仍有阻塞")
    k = counts(p)
    print(json.dumps(k, ensure_ascii=False, indent=1))
    if problems:
        print("\n问题：", file=sys.stderr)
        for x in problems:
            print(" - " + x, file=sys.stderr)
        return 1
    print("\n清单一致。")
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    pi = sub.add_parser("init")
    pi.add_argument("--branch", default="main")
    pi.add_argument("--force", action="store_true")
    pi.set_defaults(func=cmd_init)

    def add_common(sp):
        sp.add_argument("--id", required=True)
        sp.add_argument("--status")
        sp.add_argument("--acceptance")
        sp.add_argument("--next")
        sp.add_argument("--blocker")
        for f in LIST_FIELDS:
            sp.add_argument("--" + f.replace("_", "-"), dest=f, action="append")
            sp.add_argument("--replace-" + f.replace("_", "-"), dest="replace_" + f, action="store_true")

    ps = sub.add_parser("set")
    add_common(ps)
    ps.set_defaults(func=lambda a: cmd_set(a, False))

    psl = sub.add_parser("slice")
    add_common(psl)
    psl.add_argument("--slice", required=True)
    psl.set_defaults(func=lambda a: cmd_set(a, True))

    pw = sub.add_parser("show")
    pw.add_argument("--id", required=True)
    pw.set_defaults(func=cmd_show)

    pc = sub.add_parser("counts")
    pc.set_defaults(func=cmd_counts)

    pk = sub.add_parser("check")
    pk.set_defaults(func=cmd_check)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
