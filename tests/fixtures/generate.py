"""Generate BMod test fixtures (needs Pillow). Run: python3 tests/fixtures/generate.py

cube.obj / cube.glb + cube.png
    1 m cube with a numbered 3x2 UV atlas, one material. The texture is 600x400
    on purpose, to exercise power-of-two resizing.
printer.glb + printer_body.png + printer_money.png
    "Money printer": a box (material "Body") with a screen strip on the front
    (material "Money") whose texture tiles vertically, for testing scrolling.
    The box and strip are separate objects ("Printer", "Screen"), for testing bodygroups.

Everything is Y-up in metres, like a Blender export. glTF UVs use a top-left origin.
"""
import json
import struct
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent


def box_faces(sx, sy, sz, y0=0.0):
    """Six outward-facing CCW quads of an axis-aligned box centred on x/z, sitting at y0."""
    x, z = sx / 2, sz / 2
    y1 = y0 + sy
    return [
        ((1, 0, 0), [(x, y0, z), (x, y0, -z), (x, y1, -z), (x, y1, z)]),
        ((-1, 0, 0), [(-x, y0, -z), (-x, y0, z), (-x, y1, z), (-x, y1, -z)]),
        ((0, 1, 0), [(-x, y1, z), (x, y1, z), (x, y1, -z), (-x, y1, -z)]),
        ((0, -1, 0), [(-x, y0, -z), (x, y0, -z), (x, y0, z), (-x, y0, z)]),
        ((0, 0, 1), [(-x, y0, z), (x, y0, z), (x, y1, z), (-x, y1, z)]),
        ((0, 0, -1), [(x, y0, -z), (-x, y0, -z), (-x, y1, -z), (x, y1, -z)]),
    ]


class Prim:
    """Indexed triangle list. UVs given with a bottom-left origin."""

    def __init__(self):
        self.pos, self.nrm, self.uv, self.idx = [], [], [], []

    def quad(self, normal, corners, uvs):
        base = len(self.pos)
        for p, t in zip(corners, uvs):
            self.pos.append(p)
            self.nrm.append(normal)
            self.uv.append(t)
        self.idx += [base, base + 1, base + 2, base, base + 2, base + 3]


FULL_UV = [(0, 0), (1, 0), (1, 1), (0, 1)]


def write_glb(path, objects):
    """objects: list of (object name, [(material name, Prim)]). One node and mesh per
    object, one primitive per material; objects can share materials."""
    blob = b""
    views, accessors, materials, meshes = [], [], [], []

    def add(data, count, kind, comp, extra=None):
        nonlocal blob
        blob += b"\0" * (-len(blob) % 4)
        views.append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(data)})
        blob += data
        acc = {"bufferView": len(views) - 1, "componentType": comp, "count": count, "type": kind}
        acc.update(extra or {})
        accessors.append(acc)
        return len(accessors) - 1

    def prim(mat, p):
        if mat not in materials:
            materials.append(mat)
        xs = list(zip(*p.pos))
        a_pos = add(b"".join(struct.pack("<3f", *v) for v in p.pos), len(p.pos), "VEC3", 5126,
                    {"min": [min(c) for c in xs], "max": [max(c) for c in xs]})
        a_nrm = add(b"".join(struct.pack("<3f", *v) for v in p.nrm), len(p.nrm), "VEC3", 5126)
        a_uv = add(b"".join(struct.pack("<2f", u, 1 - v) for u, v in p.uv), len(p.uv), "VEC2", 5126)
        a_idx = add(b"".join(struct.pack("<H", i) for i in p.idx), len(p.idx), "SCALAR", 5123)
        return {"attributes": {"POSITION": a_pos, "NORMAL": a_nrm, "TEXCOORD_0": a_uv},
                "indices": a_idx, "material": materials.index(mat)}

    for _, prims in objects:
        meshes.append({"primitives": [prim(mat, p) for mat, p in prims]})
    blob += b"\0" * (-len(blob) % 4)

    doc = {
        "asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": list(range(len(objects)))}],
        "nodes": [{"mesh": i, "name": name} for i, (name, _) in enumerate(objects)],
        "meshes": meshes,
        "materials": [{"name": name} for name in materials],
        "buffers": [{"byteLength": len(blob)}],
        "bufferViews": views, "accessors": accessors,
    }
    js = json.dumps(doc).encode()
    js += b" " * (-len(js) % 4)
    glb = struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(blob))
    glb += struct.pack("<II", len(js), 0x4E4F534A) + js + struct.pack("<II", len(blob), 0x004E4942) + blob
    path.write_bytes(glb)


def make_cube():
    p = Prim()
    for f, (n, quad) in enumerate(box_faces(1, 1, 1)):
        u0, v0 = (f % 3) / 3, (f // 3) / 2
        p.quad(n, quad, [(u0, v0), (u0 + 1 / 3, v0), (u0 + 1 / 3, v0 + .5), (u0, v0 + .5)])

    lines = [f"v {x} {y} {z}" for x, y, z in p.pos] + [f"vt {u} {v}" for u, v in p.uv]
    lines += [f"vn {x} {y} {z}" for x, y, z in p.nrm]
    lines += ["f " + " ".join(f"{i + 1}/{i + 1}/{i + 1}" for i in p.idx[k:k + 3]) for k in range(0, len(p.idx), 3)]
    (OUT / "cube.obj").write_text("\n".join(lines) + "\n")
    write_glb(OUT / "cube.glb", [("cube", [("Cube", p)])])

    img = Image.new("RGB", (600, 400))
    d = ImageDraw.Draw(img)
    colors = ["#e74c3c", "#2ecc71", "#3498db", "#f1c40f", "#9b59b6", "#e67e22"]
    for f in range(6):
        x0, y0 = (f % 3) * 200, (1 - f // 3) * 200
        d.rectangle([x0, y0, x0 + 199, y0 + 199], fill=colors[f], outline="black", width=6)
        d.text((x0 + 90, y0 + 90), str(f + 1), fill="white", font_size=48)
    img.save(OUT / "cube.png")


def make_printer():
    # 0.6 wide, 0.35 tall, 0.4 deep. Front is +Z in glTF (-Y in Blender).
    body = Prim()
    for n, quad in box_faces(0.6, 0.35, 0.4):
        body.quad(n, quad, FULL_UV)

    # Screen strip just proud of the front face.
    z = 0.2 + 0.002
    money = Prim()
    money.quad((0, 0, 1), [(-0.2, 0.08, z), (0.2, 0.08, z), (0.2, 0.28, z), (-0.2, 0.28, z)], FULL_UV)
    write_glb(OUT / "printer.glb", [("Printer", [("Body", body)]), ("Screen", [("Money", money)])])

    img = Image.new("RGB", (512, 512), "#3b3f45")
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, 511, 511], outline="#1d1f22", width=16)
    for y in (128, 256, 384):
        d.line([(16, y), (495, y)], fill="#2a2d31", width=6)
    for x, y in ((40, 40), (472, 40), (40, 472), (472, 472)):
        d.ellipse([x - 10, y - 10, x + 10, y + 10], fill="#8a9099")
    d.text((180, 230), "BMOD", fill="#c8a43b", font_size=48)
    img.save(OUT / "printer_body.png")

    # Tiles vertically: two bills per 256 px, so the scroll loops seamlessly.
    img = Image.new("RGB", (256, 256), "#0b3d1a")
    d = ImageDraw.Draw(img)
    for y in (16, 144):
        d.rectangle([24, y, 231, y + 96], fill="#85bb65", outline="#2e5e1e", width=6)
        d.ellipse([100, y + 20, 156, y + 76], fill="#4f7a3a")
        d.text((112, y + 30), "$", fill="#e8f5d8", font_size=36)
        d.text((36, y + 10), "100", fill="#2e5e1e", font_size=18)
    img.save(OUT / "printer_money.png")


if __name__ == "__main__":
    make_cube()
    make_printer()
    print("fixtures written to", OUT)
