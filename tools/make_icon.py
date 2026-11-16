"""Generates assets/icon.ico (multi-size). Requires: pip install PySide6-Essentials"""
import struct, sys
from pathlib import Path
from PySide6.QtCore import QRectF, Qt, QByteArray, QBuffer, QIODevice
from PySide6.QtGui import QGuiApplication, QImage, QPainter, QColor, QLinearGradient

app = QGuiApplication(sys.argv)

def render(n):
    img = QImage(n, n, QImage.Format_ARGB32_Premultiplied)
    img.fill(Qt.transparent)
    p = QPainter(img)
    p.setRenderHint(QPainter.Antialiasing)
    g = QLinearGradient(0, 0, 0, n)
    g.setColorAt(0, QColor("#2a2a31")); g.setColorAt(1, QColor("#0b0b0d"))
    p.setBrush(g); p.setPen(Qt.NoPen)
    p.drawRoundedRect(QRectF(0, 0, n, n), n * 0.23, n * 0.23)
    # notch pill
    p.setBrush(QColor("#000000"))
    pw, ph = n * 0.74, n * 0.36
    p.drawRoundedRect(QRectF((n - pw) / 2, n * 0.32, pw, ph), ph / 2, ph / 2)
    # bars
    acc = QLinearGradient(0, n * 0.3, 0, n * 0.7)
    acc.setColorAt(0, QColor("#ff9a5a")); acc.setColorAt(1, QColor("#ff4d8d"))
    p.setBrush(acc)
    bw = n * 0.075
    heights = (0.16, 0.26, 0.20, 0.12)
    x0 = n / 2 - (bw * 7) / 2
    for i, h in enumerate(heights):
        hh = max(bw, n * h)
        p.drawRoundedRect(QRectF(x0 + i * bw * 2, n * 0.5 - hh / 2, bw, hh), bw / 2, bw / 2)
    p.end()
    ba = QByteArray(); buf = QBuffer(ba); buf.open(QIODevice.WriteOnly)
    img.save(buf, "PNG")
    return bytes(ba)

sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
pngs = [render(s) for s in sizes]
out = bytearray(struct.pack("<HHH", 0, 1, len(sizes)))
offset = 6 + 16 * len(sizes)
for s, data in zip(sizes, pngs):
    out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
for data in pngs:
    out += data
root = Path(__file__).resolve().parent.parent
(root / "assets" / "icon.ico").write_bytes(out)
(root / "assets" / "icon.png").write_bytes(pngs[-1])
print("wrote assets/icon.ico")
