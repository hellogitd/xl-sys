"""生成 XL-OS 桌面 UI 概念图（中式视觉语言）。"""

from __future__ import annotations

import os
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "ui-concept.png")

W, H = 1800, 1100

# ---- 色板：中国传统色（低饱和）
YUEBAI = "#F4F6F3"   # 月白
WINDOW = "#FDFDFC"   # 纸白
LINE = "#E2E5DF"     # 极淡线
INK = "#1F2328"      # 墨
INK2 = "#6B7280"     # 次要
DAILAN = "#3D5A80"   # 黛蓝（强调）
ZHESHI = "#9A5B3D"   # 赭石（警示/点缀）
SEL = "#E9EEF5"      # 选中底（黛蓝淡化）

TITLE_FONT = "C:/Windows/Fonts/simsun.ttc"   # 宋体（模拟思源宋体）
BODY_FONT = "C:/Windows/Fonts/msyh.ttc"      # 雅黑（模拟思源黑体）


def font(path: str, size: int, index: int = 0):
    if os.path.exists(path):
        return ImageFont.truetype(path, size, index=index)
    return ImageFont.load_default()


def title_font(s): return font(TITLE_FONT, s)
def body_font(s): return font(BODY_FONT, s)


img = Image.new("RGB", (W, H), YUEBAI)
d = ImageDraw.Draw(img)


def text(x, y, s, f, fill):
    d.text((x, y), s, font=f, fill=fill)


def cxtext(cx, cy, s, f, fill):
    b = d.textbbox((0, 0), s, font=f)
    d.text((cx - (b[2] - b[0]) / 2, cy - (b[3] - b[1]) / 2 - b[1]), s, font=f, fill=fill)


# ============================================================ 顶栏
TOP_H = 36
d.rectangle([0, 0, W, TOP_H], fill="#EDF0EC")
d.line([0, TOP_H, W, TOP_H], fill=LINE, width=1)

ft = body_font(13)
text(28, 10, "14:32", ft, INK)
text(80, 10, "十一月八日 · 周日", ft, INK2)
cxtext(W / 2, TOP_H / 2, "文件管理器", body_font(13), INK2)

# 右侧状态：网/音/电/AI
sx = W - 260
for i, label in enumerate(["网", "音", "电"]):
    d.rounded_rectangle([sx + i * 44, 10, sx + i * 44 + 26, 26], radius=4, outline=LINE, width=1)
    cxtext(sx + i * 44 + 13, 18, label, body_font(11), INK2)
d.rounded_rectangle([sx + 148, 9, sx + 190, 27], radius=4, fill=DAILAN)
cxtext(sx + 169, 18, "AI", body_font(11), "#FFFFFF")

# ============================================================ 后置窗口：内核日志
wx, wy, ww, wh = 80, 70, 860, 460
d.rounded_rectangle([wx + 4, wy + 6, wx + ww + 4, wy + wh + 6], radius=8, fill="#E9EBE5")
d.rounded_rectangle([wx, wy, wx + ww, wy + wh], radius=6, fill=WINDOW, outline=LINE, width=1)
d.rounded_rectangle([wx, wy, wx + ww, wy + 40], radius=6, fill="#F6F7F4")
d.line([wx, wy + 40, wx + ww, wy + 40], fill=LINE, width=1)
text(wx + 18, wy + 12, "内核日志", title_font(15), INK)
# 三个窗口按钮
for i, sym in enumerate(["−", "□", "×"]):
    bx = wx + ww - 92 + i * 28
    d.rounded_rectangle([bx, wy + 13, bx + 20, wy + 27], radius=3, outline=LINE, width=1)
    cxtext(bx + 10, wy + 19, sym, body_font(11), INK2)

logs = [
    ("14:28:03", "内核", "启动完成，4 核已就绪", INK2),
    ("14:28:05", "服务", "文件系统服务已挂载 / 主分区", INK2),
    ("14:29:41", "警告", "进程 812「浏览器渲染进程」虚拟内存接近上限", ZHESHI),
    ("14:29:41", "内核", "已回收 128 MiB 缓存页", INK2),
    ("14:30:12", "AI", "本地模型已加载（3.8B，量化 Q4）", DAILAN),
]
fl = body_font(13)
ly = wy + 60
for t, tag, msg, color in logs:
    text(wx + 20, ly, t, fl, "#9AA0A6")
    text(wx + 92, ly, tag, body_font(12), color)
    text(wx + 138, ly, msg, fl, INK if color == INK2 else color)
    ly += 28

# ============================================================ 前置窗口：文件管理器
fx, fy, fw, fh = 380, 310, 960, 610
d.rounded_rectangle([fx + 5, fy + 8, fx + fw + 5, fy + fh + 8], radius=8, fill="#E4E7E0")
d.rounded_rectangle([fx, fy, fx + fw, fy + fh], radius=6, fill=WINDOW, outline=LINE, width=1)
d.rounded_rectangle([fx, fy, fx + fw, fy + 42], radius=6, fill="#F6F7F4")
d.line([fx, fy + 42, fx + fw, fy + 42], fill=LINE, width=1)
text(fx + 20, fy + 13, "文件管理器", title_font(16), INK)
for i, sym in enumerate(["−", "□", "×"]):
    bx = fx + fw - 96 + i * 28
    d.rounded_rectangle([bx, fy + 14, bx + 20, fy + 28], radius=3, outline=LINE, width=1)
    cxtext(bx + 10, fy + 21, sym, body_font(11), INK2)

# 面包屑
by = fy + 58
text(fx + 22, by, "主目录", body_font(13), INK2)
text(fx + 84, by, "/", body_font(13), "#B9BEC6")
text(fx + 100, by, "项目", body_font(13), INK2)
text(fx + 136, by, "/", body_font(13), "#B9BEC6")
text(fx + 152, by, "xl-op-sys", body_font(13), INK)

# 列表表头
hy = fy + 96
d.line([fx + 20, hy + 22, fx + fw - 20, hy + 22], fill=LINE, width=1)
heads = [("名称", 20), ("修改时间", 560), ("大小", 720), ("类型", 840)]
for h, hx in heads:
    text(fx + hx, hy, h, body_font(12), INK2)

files = [
    ("boot", "今天 14:28", "—", "文件夹", True),
    ("kernel", "今天 14:28", "—", "文件夹", True),
    ("userland", "今天 14:27", "—", "文件夹", True),
    ("tools", "昨天 22:10", "—", "文件夹", True),
    ("ARCHITECTURE.md", "今天 13:46", "12.4 KB", "Markdown", False),
    ("BLUEPRINT.md", "今天 14:22", "18.1 KB", "Markdown", False),
    ("DECISIONS.md", "今天 14:15", "9.6 KB", "Markdown", False),
    ("POSITIONING.md", "今天 13:58", "7.2 KB", "Markdown", False),
    ("REFERENCES.md", "今天 14:05", "5.8 KB", "Markdown", False),
]
row_h = 42
ry = hy + 30
for i, (name, mtime, size, ftype, is_dir) in enumerate(files):
    if i == 4:  # 选中行
        d.rounded_rectangle([fx + 14, ry - 4, fx + fw - 14, ry + row_h - 10], radius=4, fill=SEL)
    # 图标：文件夹用赭石细框，文件用极淡方块
    ix = fx + 24
    if is_dir:
        d.rounded_rectangle([ix, ry + 6, ix + 18, ry + 22], radius=3, outline="#C4A87A", width=1)
        d.rectangle([ix + 2, ry + 3, ix + 9, ry + 7], fill="#C4A87A")
    else:
        d.rounded_rectangle([ix + 2, ry + 3, ix + 16, ry + 22], radius=2, outline="#C9CED6", width=1)
    text(fx + 58, ry + 4, name, body_font(14), INK if i == 4 else "#33383F")
    text(fx + 560, ry + 5, mtime, body_font(12), INK2)
    text(fx + 720, ry + 5, size, body_font(12), INK2)
    text(fx + 840, ry + 5, ftype, body_font(12), INK2)
    ry += row_h

# 底栏状态
sy = fy + fh - 34
d.line([fx + 20, sy, fx + fw - 20, sy], fill=LINE, width=1)
text(fx + 20, sy + 10, "9 个项目", body_font(12), INK2)
text(fx + fw - 160, sy + 10, "剩余 218 GiB", body_font(12), INK2)

# ============================================================ AI 侧栏
ax, ay, aw, ah = 1470, 70, 300, 830
d.rounded_rectangle([ax, ay, ax + aw, ay + ah], radius=6, fill="#F0F2EE", outline=LINE, width=1)
text(ax + 22, ay + 20, "AI", title_font(17), DAILAN)
d.line([ax + 22, ay + 56, ax + aw - 22, ay + 56], fill=LINE, width=1)

# 用户气泡
b1x, b1y, b1w = ax + 40, ay + 82, 240
d.rounded_rectangle([b1x, b1y, b1x + b1w, b1y + 74], radius=5, fill=WINDOW, outline=LINE, width=1)
text(b1x + 12, b1y + 10, "把下载文件夹里", body_font(13), INK)
text(b1x + 12, b1y + 32, "上周的截图", body_font(13), INK)
text(b1x + 12, b1y + 54, "按日期归类", body_font(13), INK)

# AI 气泡
b2x, b2y, b2w = ax + 18, b1y + 96, 262
d.rounded_rectangle([b2x, b2y, b2x + b2w, b2y + 168], radius=5, fill="#E9EEF5", outline="#D3DCE8", width=1)
text(b2x + 12, b2y + 10, "将移动 23 个文件。", body_font(13), INK)
text(b2x + 12, b2y + 32, "预计影响：无删除。", body_font(13), INK)
text(b2x + 12, b2y + 54, "需要权限：", body_font(12), INK2)
text(b2x + 12, b2y + 74, "· 读取 下载/", body_font(12), DAILAN)
text(b2x + 12, b2y + 94, "· 写入 下载/截图/", body_font(12), DAILAN)

# 按钮
btn_y = b2y + 122
d.rounded_rectangle([b2x + 12, btn_y, b2x + 82, btn_y + 30], radius=4, fill=DAILAN)
cxtext(b2x + 47, btn_y + 15, "执行", body_font(13), "#FFFFFF")
d.rounded_rectangle([b2x + 92, btn_y, b2x + 192, btn_y + 30], radius=4, outline=LINE, width=1, fill=WINDOW)
cxtext(b2x + 142, btn_y + 15, "预览命令", body_font(12), INK2)

# 输入框
iy = ay + ah - 68
d.rounded_rectangle([ax + 18, iy, ax + aw - 18, iy + 46], radius=5, fill=WINDOW, outline=LINE, width=1)
text(ax + 34, iy + 13, "说点什么，或输入 / 看命令", body_font(13), "#A8AEB8")

# ============================================================ Dock
dw = 560
dx = (W - dw) / 2
dy = H - 92
d.rounded_rectangle([dx, dy, dx + dw, dy + 60], radius=8, fill=WINDOW, outline=LINE, width=1)

dock_items = [("文件", True), ("终端", False), ("浏览器", False), ("设置", False), ("AI", False)]
item_w = dw / len(dock_items)
for i, (name, active) in enumerate(dock_items):
    icx = dx + item_w * i + item_w / 2
    # 图标：细线方块 + 内部极简笔画
    s = 22
    d.rounded_rectangle([icx - s / 2, dy + 10, icx + s / 2, dy + 10 + s], radius=4,
                        outline=DAILAN if active else "#9AA0A6", width=1)
    if active:
        d.line([icx - 7, dy + 21, icx + 7, dy + 21], fill=DAILAN, width=1)
        d.line([icx - 7, dy + 25, icx + 3, dy + 25], fill=DAILAN, width=1)
    cxtext(icx, dy + 44, name, body_font(11), DAILAN if active else INK2)

# ============================================================ 脚注
cxtext(W / 2, H - 14,
       "XL-OS 桌面概念 · 中式视觉语言：月白底 · 墨色字 · 黛蓝强调 · 宋体标题 · 1px 细线 · 大留白",
       body_font(13), "#8A9099")

img.save(OUT, "PNG")
print("saved:", OUT, img.size)
