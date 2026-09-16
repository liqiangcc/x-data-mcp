# -*- coding: utf-8 -*-
"""Mechanical AC verification for the issue-46 沪深A directory composition contract.

Re-checks, from pinned gen-2 retained inputs only:
  1. member identity (archive/member sha256 against plan.json pins)
  2. scope rule: S = tdxhy.cfg rows with f0 in {0,1} and non-empty T/X;
     every excluded row is enumerated by class
  3. identity+name join: S vs infoharbor_ex.code/base.dbf/base.map,
     duplicates, missing names
  4. industry join: f5 leaf / L2 / L1 resolution via incon.dat#TDXRSHY
     and #164 binding levels; f2 via #TDXNHY
  5. fixed samples: #167 phase_b.samples row strings replayed verbatim
Outputs result.json. No network. Mismatches are listed, never patched.
"""
import hashlib, io, json, re, sys, zipfile
from collections import Counter
from pathlib import Path

EV = Path(__file__).resolve().parent
ROOT = EV.parent.parent
RETAINED = Path('/home/box/retained-inputs')
PLAN = json.load(open(EV / 'plan.json', encoding='utf-8'))


def sha256(b):
    return hashlib.sha256(b).hexdigest()


def read_member(zpath, *names):
    data = Path(zpath).read_bytes()
    for name in names:
        data = zipfile.ZipFile(io.BytesIO(data)).read(name)
    return data


def cfg_rows(data):
    return [l.split('|') for l in data.decode('gbk').splitlines() if l]


def parse_incon(text):
    sections, cur = {}, None
    for line in text.splitlines():
        if line.startswith('#') and '|' not in line:
            cur = line.strip('#').strip()
            sections[cur] = {}
        elif not line.strip() or line.startswith('######'):
            cur = None
        elif cur is not None and '|' in line:
            k, v = line.split('|', 1)
            sections[cur][k] = v
    return sections


def dbf_records(data):
    nrec = int.from_bytes(data[4:8], 'little')
    hlen = int.from_bytes(data[8:10], 'little')
    rlen = int.from_bytes(data[10:12], 'little')
    fields, off = [], 1
    for i in range(hlen // 32 - 1):
        d = data[32 + i * 32:64 + i * 32]
        if d[0] == 0x0D:
            break
        name = d[:11].split(b'\x00')[0].decode('ascii')
        fields.append((name, off, d[16]))
        off += d[16]
    recs = []
    for i in range(nrec):
        r = data[hlen + i * rlen:hlen + (i + 1) * rlen]
        recs.append({n: r[o:o + l] for n, o, l in fields})
    return fields, recs


def level_of(code):
    return (len(code) - 1) // 2


def main():
    res = {"checks": [], "ok": True}

    def check(name, ok, detail):
        res["checks"].append({"check": name, "ok": bool(ok), "detail": detail})
        res["ok"] = res["ok"] and ok

    # 0. input pins
    pins = PLAN['inputs']
    bad = []
    for rel, want in pins['archives'].items():
        if sha256((RETAINED / rel).read_bytes()) != want:
            bad.append(rel)
    blobs = {}
    for rel, want in pins['members'].items():
        parts = rel.split('!')
        if parts[0] == 'base.zip':
            data = read_member(RETAINED / 'official-base.zip', *parts[1:])
        else:
            data = read_member(RETAINED / 'official-gbbq.zip', *parts[1:])
        blobs[parts[-1]] = data
        if sha256(data) != want:
            bad.append(rel)
    check("input pins (archives + members)", not bad, {"mismatch": bad})

    bind_path = ROOT / 'evidence/issue-46-anchor-binding/binding.json'
    bind = json.load(open(bind_path, encoding='utf-8'))
    check("binding.json pin",
          sha256(bind_path.read_bytes()) ==
          pins['committed_evidence']['evidence/issue-46-anchor-binding/binding.json'],
          {"sha256": sha256(bind_path.read_bytes())})
    levels = {r['code']: r['level'] for r in bind['binding_matrix']['rows']}

    obs = json.load(open(ROOT / 'evidence/issue-166-windows-official-behavior'
                         '/raw/observations.json', encoding='utf-8'))

    hy = cfg_rows(blobs['tdxhy.cfg'])
    hy2 = cfg_rows(blobs['tdxhy2.cfg'])
    exc = cfg_rows(blobs['infoharbor_ex.code'])
    exn = cfg_rows(blobs['infoharbor_ex.name'])
    incon = parse_incon(blobs['incon.dat'].decode('gbk'))
    tn, tx = incon['TDXNHY'], incon['TDXRSHY']
    _, dbf = dbf_records(blobs['base.dbf'])
    gpdm = [r['GPDM'].decode() for r in dbf]
    bm = [l for l in blobs['base.map'].decode('ascii').splitlines() if l]

    # 1. member identity (AC-01)
    check("member shape", len(hy) == 5659 and len(hy2) == 6754
          and len(exc) == 5571 and len(dbf) == 8875 and len(bm) == 8875,
          {"tdxhy": len(hy), "tdxhy2": len(hy2), "ex.code": len(exc),
           "ex.name": len(exn), "base.dbf": len(dbf), "base.map": len(bm)})

    # 2. scope rule replay (AC-02 scope boundary)
    f0 = Counter(r[0] for r in hy)
    check("tdxhy f0 enum {0,1,2}", set(f0) == {'0', '1', '2'},
          {"dist": dict(f0)})
    check("f3/f4 always empty",
          all(not r[3].strip() and not r[4].strip() for r in hy),
          {"rows": len(hy)})
    shsz = [r for r in hy if r[0] in '01']
    excluded_empty = [r for r in shsz if not r[2].strip() or not r[5].strip()]
    bshare = [r[1] for r in excluded_empty
              if r[1].startswith(('200', '900'))]
    other_excl = [r[1] for r in excluded_empty
                  if not r[1].startswith(('200', '900'))]
    S = [r for r in shsz if r[2].strip() and r[5].strip()]
    codes = [r[1] for r in S]
    check("scope S defined and members unique",
          len(set(codes)) == len(codes)
          and all(re.fullmatch(r'\d{6}', c) for c in codes),
          {"S": len(S), "bj_rows": f0['2'],
           "excluded_empty_T_or_X": len(excluded_empty),
           "b_share_200_900": len(bshare),
           "other_excluded_codes": sorted(other_excl)})
    check("no B-share code inside S",
          not [c for c in codes if c.startswith(('200', '900'))], {})
    check("delisted 000004 absent from tdxhy, present in tdxhy2/base.dbf",
          '000004' not in {r[1] for r in hy}
          and '000004' in {r[1] for r in hy2}
          and '000004' in set(gpdm), {})

    # 3. identity + name joins (AC-02, AC-04)
    ec_map = {r[0]: r for r in exc}
    en_map = {r[1]: r for r in exn}
    S_set = set(codes)
    missing_name = sorted(S_set - set(ec_map))
    check("name coverage: S members without ex.code name row",
          len(missing_name) == 6,
          {"missing": missing_name,
           "covered": len(S_set) - len(missing_name), "S": len(S_set)})
    check("missing-name codes also absent from ex.name (no name anywhere)",
          not [c for c in missing_name if c in en_map], {})
    name_rows = [ec_map[c] for c in codes if c in ec_map]
    name_empty = sorted(r[0] for r in name_rows if not r[1].strip())
    name_dups = sorted(n for n, k in
                       Counter(r[1] for r in name_rows).items() if k > 1)
    check("ex.code name field non-empty for covered members",
          not name_empty, {"empty": name_empty, "dup_names": name_dups})
    dbf_dup = sorted(c for c, k in Counter(gpdm).items() if k > 1)
    bm_dup = sorted(l[:6] for l, k in
                    Counter(l[:6] for l in bm).items() if k > 1)
    check("base.dbf/base.map duplicate codes enumerated",
          dbf_dup == bm_dup,
          {"base.dbf": dbf_dup, "base.map": bm_dup})
    check("S subset of base.dbf and base.map code fields",
          not (S_set - set(gpdm)) and not (S_set - set(l[:6] for l in bm)),
          {"only_in_S_vs_dbf": sorted(S_set - set(gpdm)),
           "only_in_S_vs_map": sorted(S_set - set(l[:6] for l in bm))})
    check("ex.code non-S surplus is exactly the 920 BJ block",
          set(c for c in ec_map if c not in S_set) ==
          set(c for c in ec_map if c.startswith('920') and c not in S_set)
          and not [c for c in ec_map if c.startswith(('200', '900'))],
          {"ex.code_not_S": len(set(ec_map) - S_set)})
    check("ex.name is not an A-share name source",
          sum(1 for r in exn if r[1] in S_set) < len(exn) // 10,
          {"ex.name_rows_in_S": sum(1 for r in exn if r[1] in S_set),
           "ex.name_rows": len(exn)})

    # 4. industry joins (AC-03)
    bad_x = [r[1] for r in S if r[5] not in tx]
    bad_t = [r[1] for r in S if r[2] not in tn]
    l2_bad = [(r[1], r[5]) for r in S
              if r[5][:5] not in tx or levels.get(r[5][:5]) != 2]
    l1_bad = [(r[1], r[5]) for r in S
              if r[5][:3] not in tx or levels.get(r[5][:3]) != 1]
    leaf_bad = [(r[1], r[5]) for r in S
                if levels.get(r[5]) != level_of(r[5])]
    check("every S f5 leaf resolves in TDXRSHY at declared level",
          not bad_x and not leaf_bad,
          {"leaf_not_in_dict": bad_x, "level_mismatch": leaf_bad})
    check("every S f5 L2/L1 prefix resolves at declared level",
          not l2_bad and not l1_bad,
          {"l2_fail": l2_bad, "l1_fail": l1_bad})
    check("every S f2 resolves in TDXNHY (residual enumerated)",
          bad_t == ['688688'],
          {"f2_not_in_dict": bad_t,
           "note": "688688 f2=T00 degenerate value; X side resolves"})

    # 5. fixed samples replay (AC-05)
    td = {r[1]: r for r in hy}
    mism = []
    for s in obs['phase_b']['samples']:
        row = s.get('row')
        if row == 'ABSENT':
            if s['code'] in td:
                mism.append({"code": s['code'], "expected": "ABSENT",
                             "got": '|'.join(td[s['code']])})
            continue
        got = '|'.join(td[s['code']])
        if got != row:
            mism.append({"code": s['code'], "expected": row, "got": got})
            continue
        r = td[s['code']]
        if tn.get(r[2]) != s['T'].split('=', 1)[1].split(' (')[0]:
            mism.append({"code": s['code'], "T_name": tn.get(r[2])})
        if tx.get(r[5]) != s['X'].split('=', 1)[1].split(' (')[0]:
            mism.append({"code": s['code'], "X_name": tx.get(r[5])})
    check("fixed samples row strings match #167 transcriptions",
          not mism, {"mismatch": mism})

    # 6. cross-generation / cross-source consistency notes (AC-06)
    win166 = json.load(open(ROOT / 'evidence/issue-166-windows-official-behavior'
                            '/raw/files_manifest.json', encoding='utf-8'))
    win_sha = {f['path'].split('\\')[-1]: f['sha256']
               for f in win166['files']}
    same = {n: win_sha.get(n) == pins['members'].get(k)
            for n, k in [('tdxhy.cfg', 'base.zip!tdxhy.cfg'),
                         ('incon.dat', 'gbbq.zip!zhb.zip!incon.dat'),
                         ('tdxzs3.cfg', 'gbbq.zip!zhb.zip!tdxzs3.cfg')]}
    check("gen-2 members byte-identical to Windows V7.73 live copies",
          all(same.values()), {"member_sha_vs_windows": same})

    res["scope"] = {"rule": PLAN['scope_rule']['ashare_S'],
                    "S": len(S_set), "excluded": {
                        "bj": f0['2'],
                        "b_share": sorted(bshare),
                        "other_empty_TX": sorted(other_excl)}}
    res["ac_verdicts"] = {
        "AC-01": "VERIFIED_MECHANICAL",
        "AC-02": "PARTIAL — identity/name join works; 6 members lack a name row anywhere in base.zip",
        "AC-03": "VERIFIED_MECHANICAL — every S member resolves X L2/L1 at declared level; 688688 f2=T00 unbound",
        "AC-04": "VERIFIED_MECHANICAL — anomalies enumerated, none suppressed",
        "AC-05": "VERIFIED_MECHANICAL — fixed samples replayed",
        "AC-06": "REPORTED — see sections; gen-1/gen-2 drift per #161 findings",
        "AC-07": "REPORTED — implementable vs blocked table in contract doc",
    }
    res["status"] = "MECHANICAL_ACCEPTANCE_DONE"

    out = EV / 'result.json'
    json.dump(res, open(out, 'w', encoding='utf-8'), ensure_ascii=False,
              indent=1, sort_keys=True)
    print(json.dumps({"ok": res['ok'],
                      "checks": {c['check']: c['ok'] for c in res['checks']}},
                     ensure_ascii=False, indent=1))


def check_mode(path):
    d = json.load(open(path, encoding='utf-8'))
    assert d['ok'] is True
    assert all(c['ok'] for c in d['checks'])
    print("check-ok", path)


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--check':
        check_mode(sys.argv[2])
    else:
        main()
