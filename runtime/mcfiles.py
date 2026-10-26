#!/usr/bin/env python3
"""Write <ver>.files ("url path" per line: client jar, libraries, linux natives,
assets) and <ver>.launch (classpath, natives, main class) for a vanilla client.

Only URLs on Mojang's own servers are recorded; mc-install downloads them on
the box. Natives are the 32-bit Linux ones: the game runs on a 32-bit JVM."""
import json, sys, urllib.request
ver = sys.argv[1]
man = json.load(urllib.request.urlopen("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"))
vurl = next(v["url"] for v in man["versions"] if v["id"] == ver)
vj = json.load(urllib.request.urlopen(vurl))
out, cp, natives = [], [], []
out.append((vurl, f"versions/{ver}/{ver}.json"))
out.append((vj["downloads"]["client"]["url"], f"versions/{ver}/{ver}.jar"))
def allowed(lib):
    ok = True
    for r in lib.get("rules", []):
        osn = r.get("os", {}).get("name")
        if osn is None or osn == "linux":
            ok = r["action"] == "allow"
        elif r["action"] == "allow" and not any(x.get("os", {}).get("name") in (None, "linux") for x in lib["rules"]):
            ok = False
    return ok
for lib in vj["libraries"]:
    if not allowed(lib):
        continue
    d = lib.get("downloads", {})
    if "artifact" in d:
        a = d["artifact"]; out.append((a["url"], "libraries/" + a["path"])); cp.append("libraries/" + a["path"])
    nat = lib.get("natives", {}).get("linux")
    if nat:
        nat = nat.replace("${arch}", "32")
        a = d["classifiers"][nat]; out.append((a["url"], "libraries/" + a["path"])); natives.append("libraries/" + a["path"])
ai = vj["assetIndex"]
out.append((ai["url"], f"assets/indexes/{ai['id']}.json"))
idx = json.load(urllib.request.urlopen(ai["url"]))
for name, o in idx["objects"].items():
    h = o["hash"]
    out.append((f"https://resources.download.minecraft.net/{h[:2]}/{h}", f"assets/objects/{h[:2]}/{h}"))
with open(f"{ver}.files", "w") as f:
    for u, p in out:
        f.write(f"{u} {p}\n")
with open(f"{ver}.launch", "w") as f:
    f.write("CP=" + ":".join(cp + [f"versions/{ver}/{ver}.jar"]) + "\n")
    f.write('NATIVES="' + " ".join(natives) + '"\n')
    f.write(f"MAIN={vj['mainClass']}\nASSET_INDEX={ai['id']}\n")
    f.write("ARGS='" + vj.get("minecraftArguments", "") + "'\n")
print(len(out), "files;", sum(o['size'] for o in idx['objects'].values())//1048576, "MB assets", file=sys.stderr)
