#!/usr/bin/env python3
"""Fetches pinned Firebase Cloud Messaging Android AAR/JAR dependencies without Gradle,
extracts classes.jar & resources, generates optional google_services.xml from
google-services.json, and merges AAR AndroidManifest.xml components for aapt2/d8 (MS-443).
"""
import argparse
import concurrent.futures
import json
import os
import pathlib
import shutil
import sys
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

REPOS = (
    "https://dl.google.com/dl/android/maven2",
    "https://repo1.maven.org/maven2",
)

# Transitive closure of com.google.firebase:firebase-messaging:24.1.1 (compile + runtime).
ARTIFACTS = [
    ("androidx.activity", "activity", "1.0.0", "aar"),
    ("androidx.annotation", "annotation", "1.5.0", "jar"),
    ("androidx.arch.core", "core-common", "2.1.0", "jar"),
    ("androidx.arch.core", "core-runtime", "2.0.0", "aar"),
    ("androidx.asynclayoutinflater", "asynclayoutinflater", "1.0.0", "aar"),
    ("androidx.collection", "collection", "1.1.0", "jar"),
    ("androidx.concurrent", "concurrent-futures", "1.1.0", "jar"),
    ("androidx.coordinatorlayout", "coordinatorlayout", "1.0.0", "aar"),
    ("androidx.core", "core", "1.2.0", "aar"),
    ("androidx.cursoradapter", "cursoradapter", "1.0.0", "aar"),
    ("androidx.customview", "customview", "1.0.0", "aar"),
    ("androidx.documentfile", "documentfile", "1.0.0", "aar"),
    ("androidx.drawerlayout", "drawerlayout", "1.0.0", "aar"),
    ("androidx.fragment", "fragment", "1.1.0", "aar"),
    ("androidx.interpolator", "interpolator", "1.0.0", "aar"),
    ("androidx.legacy", "legacy-support-core-ui", "1.0.0", "aar"),
    ("androidx.legacy", "legacy-support-core-utils", "1.0.0", "aar"),
    ("androidx.lifecycle", "lifecycle-common", "2.1.0", "jar"),
    ("androidx.lifecycle", "lifecycle-livedata", "2.0.0", "aar"),
    ("androidx.lifecycle", "lifecycle-livedata-core", "2.0.0", "aar"),
    ("androidx.lifecycle", "lifecycle-runtime", "2.1.0", "aar"),
    ("androidx.lifecycle", "lifecycle-viewmodel", "2.1.0", "aar"),
    ("androidx.loader", "loader", "1.0.0", "aar"),
    ("androidx.localbroadcastmanager", "localbroadcastmanager", "1.0.0", "aar"),
    ("androidx.print", "print", "1.0.0", "aar"),
    ("androidx.savedstate", "savedstate", "1.0.0", "aar"),
    ("androidx.slidingpanelayout", "slidingpanelayout", "1.0.0", "aar"),
    ("androidx.swiperefreshlayout", "swiperefreshlayout", "1.0.0", "aar"),
    ("androidx.versionedparcelable", "versionedparcelable", "1.1.0", "aar"),
    ("androidx.viewpager", "viewpager", "1.0.0", "aar"),
    ("com.google.android.datatransport", "transport-api", "3.1.0", "aar"),
    ("com.google.android.datatransport", "transport-backend-cct", "3.1.9", "aar"),
    ("com.google.android.datatransport", "transport-runtime", "3.1.9", "aar"),
    ("com.google.android.gms", "play-services-base", "18.1.0", "aar"),
    ("com.google.android.gms", "play-services-basement", "18.3.0", "aar"),
    ("com.google.android.gms", "play-services-cloud-messaging", "17.2.0", "aar"),
    ("com.google.android.gms", "play-services-stats", "17.0.2", "aar"),
    ("com.google.android.gms", "play-services-tasks", "18.1.0", "aar"),
    ("com.google.errorprone", "error_prone_annotations", "2.26.0", "jar"),
    ("com.google.firebase", "firebase-annotations", "16.2.0", "jar"),
    ("com.google.firebase", "firebase-common", "21.0.0", "aar"),
    ("com.google.firebase", "firebase-common-ktx", "21.0.0", "aar"),
    ("com.google.firebase", "firebase-components", "18.0.0", "aar"),
    ("com.google.firebase", "firebase-datatransport", "18.2.0", "aar"),
    ("com.google.firebase", "firebase-encoders", "17.0.0", "jar"),
    ("com.google.firebase", "firebase-encoders-json", "18.0.0", "aar"),
    ("com.google.firebase", "firebase-encoders-proto", "16.0.0", "jar"),
    ("com.google.firebase", "firebase-iid-interop", "17.1.0", "aar"),
    ("com.google.firebase", "firebase-installations", "17.2.0", "aar"),
    ("com.google.firebase", "firebase-installations-interop", "17.1.1", "aar"),
    ("com.google.firebase", "firebase-measurement-connector", "19.0.0", "aar"),
    ("com.google.firebase", "firebase-messaging", "24.1.1", "aar"),
    ("com.google.guava", "listenablefuture", "1.0", "jar"),
    ("javax.inject", "javax.inject", "1", "jar"),
    ("org.jetbrains", "annotations", "13.0", "jar"),
    ("org.jetbrains.kotlin", "kotlin-stdlib", "1.8.22", "jar"),
    ("org.jetbrains.kotlin", "kotlin-stdlib-common", "1.8.22", "jar"),
    ("org.jetbrains.kotlin", "kotlin-stdlib-jdk7", "1.8.22", "jar"),
    ("org.jetbrains.kotlin", "kotlin-stdlib-jdk8", "1.8.22", "jar"),
    ("org.jetbrains.kotlinx", "kotlinx-coroutines-core-jvm", "1.6.4", "jar"),
    ("org.jetbrains.kotlinx", "kotlinx-coroutines-play-services", "1.6.4", "jar"),
]

ANDROID_NS = "http://schemas.android.com/apk/res/android"
TOOLS_NS = "http://schemas.android.com/tools"
ET.register_namespace("android", ANDROID_NS)

MERGE_META_SERVICES = {
    "com.google.firebase.components.ComponentDiscoveryService",
    "com.google.android.datatransport.runtime.backends.TransportBackendDiscovery",
}


def download_one(cache_dir: pathlib.Path, item: tuple[str, str, str, str]) -> pathlib.Path:
    group, artifact, version, ext = item
    filename = f"{group}_{artifact}_{version}.{ext}"
    dest = cache_dir / "raw" / filename
    if dest.exists() and dest.stat().st_size > 0:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".tmp")
    rel = f"{group.replace('.', '/')}/{artifact}/{version}/{artifact}-{version}.{ext}"
    last_err = None
    for repo in REPOS:
        url = f"{repo}/{rel}"
        try:
            with urllib.request.urlopen(url, timeout=30) as resp, open(tmp, "wb") as out:
                shutil.copyfileobj(resp, out)
            os.replace(tmp, dest)
            return dest
        except Exception as err:
            last_err = err
    raise RuntimeError(f"failed to fetch {group}:{artifact}:{version}.{ext}: {last_err}")


def strip_tools_attrs(elem: ET.Element) -> None:
    for key in list(elem.attrib):
        if key.startswith(f"{{{TOOLS_NS}}}"):
            del elem.attrib[key]
    for child in elem:
        strip_tools_attrs(child)


def replace_placeholders(elem: ET.Element, app_id: str) -> None:
    for k, v in list(elem.attrib.items()):
        if "${applicationId}" in v:
            elem.attrib[k] = v.replace("${applicationId}", app_id)
    for child in elem:
        replace_placeholders(child, app_id)


def merge_manifests(base_manifest: pathlib.Path, aar_manifests: list[str], app_id: str, out_path: pathlib.Path) -> None:
    base_tree = ET.parse(base_manifest)
    base_root = base_tree.getroot()
    base_app = base_root.find("application")
    assert base_app is not None, "base AndroidManifest.xml missing <application>"

    name_attr = f"{{{ANDROID_NS}}}name"
    exported_attr = f"{{{ANDROID_NS}}}exported"

    seen_perms = {el.attrib.get(name_attr) for el in base_root.findall("uses-permission")}
    seen_components: dict[tuple[str, str], ET.Element] = {}
    for child in base_app:
        nm = child.attrib.get(name_attr)
        if nm:
            seen_components[(child.tag, nm)] = child

    for raw_xml in aar_manifests:
        root = ET.fromstring(raw_xml)
        strip_tools_attrs(root)
        replace_placeholders(root, app_id)
        for perm in root.findall("uses-permission"):
            pname = perm.attrib.get(name_attr)
            if pname and pname not in seen_perms:
                seen_perms.add(pname)
                # Insert before <application>
                idx = list(base_root).index(base_app)
                base_root.insert(idx, perm)
        app = root.find("application")
        if app is None:
            continue
        for child in list(app):
            nm = child.attrib.get(name_attr, "")
            key = (child.tag, nm)
            if child.tag == "service" and nm in MERGE_META_SERVICES:
                existing = seen_components.get(key)
                if existing is None:
                    base_app.append(child)
                    seen_components[key] = child
                else:
                    existing_meta = {m.attrib.get(name_attr) for m in existing.findall("meta-data")}
                    for meta in list(child.findall("meta-data")):
                        mname = meta.attrib.get(name_attr)
                        if mname and mname not in existing_meta:
                            existing.append(meta)
                            existing_meta.add(mname)
                continue
            if key in seen_components:
                continue
            # Android 12+ / targetSdk 35 requires explicit android:exported on components with intent-filter.
            if child.find("intent-filter") is not None and exported_attr not in child.attrib:
                child.attrib[exported_attr] = "true"
            base_app.append(child)
            if nm:
                seen_components[key] = child

    out_path.parent.mkdir(parents=True, exist_ok=True)
    base_tree.write(out_path, encoding="utf-8", xml_declaration=True)


def generate_google_services_xml(gs_path: pathlib.Path, app_id: str, out_res_dir: pathlib.Path) -> bool:
    if not gs_path.is_file():
        return False
    data = json.loads(gs_path.read_text(encoding="utf-8"))
    project = data.get("project_info", {})
    clients = data.get("client", [])
    chosen = None
    for c in clients:
        pkg = c.get("client_info", {}).get("android_client_info", {}).get("package_name")
        if pkg == app_id:
            chosen = c
            break
    if chosen is None and len(clients) == 1:
        chosen = clients[0]
    if chosen is None:
        raise RuntimeError(f"google-services.json has no client matching package_name={app_id}")

    app_google_id = chosen.get("client_info", {}).get("mobilesdk_app_id", "")
    api_keys = chosen.get("api_key", [])
    api_key = api_keys[0].get("current_key", "") if api_keys else ""
    sender_id = str(project.get("project_number", ""))
    project_id = str(project.get("project_id", ""))
    bucket = str(project.get("storage_bucket", ""))

    values = {
        "google_app_id": app_google_id,
        "google_api_key": api_key,
        "gcm_defaultSenderId": sender_id,
        "project_id": project_id,
    }
    if bucket:
        values["google_storage_bucket"] = bucket

    missing = [k for k in ("google_app_id", "google_api_key", "gcm_defaultSenderId", "project_id") if not values[k]]
    if missing:
        raise RuntimeError(f"google-services.json missing required fields: {', '.join(missing)}")

    root = ET.Element("resources")
    for k, v in values.items():
        el = ET.SubElement(root, "string", {"name": k, "translatable": "false"})
        el.text = v
    values_dir = out_res_dir / "values"
    values_dir.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(root).write(values_dir / "google_services.xml", encoding="utf-8", xml_declaration=True)
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cache-dir", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--base-manifest", required=True)
    parser.add_argument("--app-id", default="dev.mailstream.gpui.poc")
    parser.add_argument("--google-services", default="")
    args = parser.parse_args()

    cache_dir = pathlib.Path(args.cache_dir)
    out_dir = pathlib.Path(args.out_dir)
    jars_dir = cache_dir / "jars"
    res_root = cache_dir / "res"
    manifests_dir = cache_dir / "manifests"
    jars_dir.mkdir(parents=True, exist_ok=True)
    res_root.mkdir(parents=True, exist_ok=True)
    manifests_dir.mkdir(parents=True, exist_ok=True)

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        paths = list(pool.map(lambda it: download_one(cache_dir, it), ARTIFACTS))

    jar_list: list[str] = []
    res_dirs: list[str] = []
    extra_packages: list[str] = []
    aar_manifests: list[str] = []

    for (group, artifact, version, ext), raw_path in zip(ARTIFACTS, paths):
        key = f"{group}_{artifact}_{version}"
        if ext == "jar":
            jar_dest = jars_dir / f"{key}.jar"
            if not jar_dest.exists():
                shutil.copy2(raw_path, jar_dest)
            jar_list.append(str(jar_dest))
            continue

        # AAR extraction (cached by artifact key)
        jar_dest = jars_dir / f"{key}.jar"
        man_dest = manifests_dir / f"{key}.xml"
        aar_res_dir = res_root / key
        marker = cache_dir / "extracted" / f"{key}.ok"
        if not marker.exists():
            marker.parent.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(raw_path) as zf:
                if "classes.jar" in zf.namelist():
                    jar_dest.write_bytes(zf.read("classes.jar"))
                if "AndroidManifest.xml" in zf.namelist():
                    man_dest.write_bytes(zf.read("AndroidManifest.xml"))
                res_members = [n for n in zf.namelist() if n.startswith("res/") and not n.endswith("/")]
                if res_members:
                    if aar_res_dir.exists():
                        shutil.rmtree(aar_res_dir)
                    for name in res_members:
                        rel = name[len("res/"):]
                        target = aar_res_dir / rel
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(zf.read(name))
            marker.write_text("ok", encoding="utf-8")

        if jar_dest.exists():
            jar_list.append(str(jar_dest))
        if man_dest.exists():
            xml_text = man_dest.read_text(encoding="utf-8")
            aar_manifests.append(xml_text)
            pkg = ET.fromstring(xml_text).attrib.get("package", "").strip()
            if aar_res_dir.is_dir() and any(aar_res_dir.rglob("*")):
                res_dirs.append(str(aar_res_dir))
                if pkg and pkg != args.app_id and pkg not in extra_packages:
                    extra_packages.append(pkg)

    merged_manifest = out_dir / "AndroidManifest.xml"
    merge_manifests(pathlib.Path(args.base_manifest), aar_manifests, args.app_id, merged_manifest)

    gs_res_dir = out_dir / "gs-res"
    gs_baked = False
    if args.google_services:
        gs_baked = generate_google_services_xml(pathlib.Path(args.google_services), args.app_id, gs_res_dir)
        if gs_baked:
            res_dirs.append(str(gs_res_dir))

    meta = {
        "jars": jar_list,
        "res_dirs": res_dirs,
        "extra_packages": extra_packages,
        "merged_manifest": str(merged_manifest),
        "google_services_baked": gs_baked,
    }
    (out_dir / "fcm-deps.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
    status = "with google-services.json baked" if gs_baked else "without google-services.json (runtime Device Sync/config.json fallback enabled)"
    print(f"Prepared {len(jar_list)} FCM/AndroidX jars, {len(res_dirs)} resource dirs, {len(extra_packages)} extra R packages ({status}).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
