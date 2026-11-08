"""生成 XL-OS 全景蓝图 PNG。

用法：python tools/make_blueprint.py
输出：blueprint-overview.png
"""

from __future__ import annotations

import os
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "blueprint-overview.png")

W, H = 1800, 1270
BG = "#F6F7FB"
INK = "#1E293B"
MUTED = "#64748B"

# ---------------------------------------------------------------- 字体
FONT_CANDIDATES = [
    ("C:/Windows/Fonts/msyh.ttc", 0),
    ("C:/Windows/Fonts/msyhbd.ttc", 0),
    ("C:/Windows/Fonts/simhei.ttf", 0),
    ("C:/Windows/Fonts/deng.ttf", 0),
    ("C:/Windows/Fonts/simsun.ttc", 0),
]


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path, index in FONT_CANDIDATES:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size, index=index)
            except Exception:
                continue
    return ImageFont.load_default()


F_TITLE = load_font(46)
F_SUB = load_font(22)
F_LAYER = load_font(21)
F_CHIP = load_font(16)
F_PHASE = load_font(19)
F_TIME = load_font(15)
F_NOTE = load_font(14)
F_LEGEND = load_font(15)

img = Image.new("RGB", (W, H), BG)
d = ImageDraw.Draw(img)


def ctext(xy, text, font, fill):
    bbox = d.textbbox((0, 0), text, font=font)
    w = bbox[2] - bbox[0]
    h = bbox[3] - bbox[1]
    d.text((xy[0] - w / 2, xy[1] - h / 2 - bbox[1]), text, font=font, fill=fill)


# ---------------------------------------------------------------- 标题
ctext((W / 2, 58), "XL-OS 全景蓝图", F_TITLE, INK)
ctext((W / 2, 108), "多子系统  ·  capability  ·  AI 代理  ·  兼容    ｜    Rust 微内核  ·  MIT 开源", F_SUB, MUTED)

# ---------------------------------------------------------------- 分层架构
LEFT_LABEL_W = 190
X0 = 60
X1 = W - 60
X_LABEL = X0 + LEFT_LABEL_W / 2
X_CHIPS_START = X0 + LEFT_LABEL_W + 24
X_CHIPS_END = X1 - 16

# (标签, 颜色, [组件...], 相对高度倍数)
LAYERS = [
    ("云端服务", "#4F46E5", ["更新分发", "账号与同步", "设备互联", "AI 云端", "应用商店", "遥测(默认关)"], 1.0),
    ("应用与交互", "#0284C7", ["原生应用", "兼容应用", "AI 代理", "中文终端", "设置"], 1.0),
    ("界面层", "#0D9488", ["桌面 Shell", "触屏 Shell", "中文输入法", "通知中心", "无障碍"], 1.0),
    ("系统服务", "#059669", [], 2.35),
    ("用户态基础", "#7C3AED", ["libc", "工具链", "包管理", "SDK", "调试器", "自举"], 1.0),
    ("微内核 (Rust)", "#DC2626", ["内存", "线程", "IPC", "capability", "中断", "定时器"], 1.0),
    ("HAL", "#475569", ["UEFI", "ACPI", "中断控制器", "时钟源", "SMP"], 1.0),
    ("硬件", "#64748B", ["x86_64", "ARM64", "树莓派", "触摸屏", "传感器"], 1.0),
]

BASE_H = 66
GAP = 11
Y_TOP = 150

# 计算总高
total_h = sum(BASE_H * mult for _, _, _, mult in LAYERS) + GAP * (len(LAYERS) - 1)
y = Y_TOP

SERVICE_SUBGROUPS = [
    ("系统服务", "#059669", ["文件", "网络", "图形合成", "音频", "更新", "安装器", "权限代理"]),
    ("兼容服务", "#D97706", ["Linux ABI", "Windows ABI", "Android 容器", "Web 运行时"]),
    ("AI 服务", "#DB2777", ["本地模型", "意图解析", "命令生成", "沙箱 dry-run", "审计日志"]),
]

for name, color, chips, mult in LAYERS:
    h = BASE_H * mult
    # 层条
    d.rounded_rectangle([X0, y, X1, y + h], radius=12, fill=color)
    # 层标签（白色，左侧）
    ctext((X_LABEL, y + h / 2), name, F_LAYER, "#FFFFFF")

    if name == "系统服务":
        # 三条子带
        pad = 8
        sub_h = (h - pad * 2 - 6 * 2) / 3
        sy = y + pad
        for sname, scolor, schips in SERVICE_SUBGROUPS:
            # 子带背景
            d.rounded_rectangle([X_CHIPS_START - 6, sy, X_CHIPS_END, sy + sub_h], radius=8, fill="#FFFFFF")
            # 子标签
            d.text((X_CHIPS_START + 4, sy + sub_h / 2 - 9), sname, font=F_CHIP, fill=scolor)
            label_w = d.textbbox((0, 0), sname, font=F_CHIP)[2] + 18
            cx0 = X_CHIPS_START + label_w + 6
            n = len(schips)
            usable = X_CHIPS_END - cx0 - 8
            gap = 8
            cw = (usable - gap * (n - 1)) / n
            for i, c in enumerate(schips):
                bx = cx0 + i * (cw + gap)
                d.rounded_rectangle([bx, sy + (sub_h - 34) / 2, bx + cw, sy + (sub_h + 34) / 2],
                                    radius=7, fill=color if False else "#F1F5F9", outline=scolor, width=1)
                ctext((bx + cw / 2, sy + sub_h / 2), c, F_CHIP, scolor)
            sy += sub_h + 6
    else:
        n = len(chips)
        usable = X_CHIPS_END - X_CHIPS_START
        gap = 10
        cw = (usable - gap * (n - 1)) / n
        for i, c in enumerate(chips):
            bx = X_CHIPS_START + i * (cw + gap)
            d.rounded_rectangle([bx, y + h / 2 - 19, bx + cw, y + h / 2 + 19],
                                radius=8, fill="#FFFFFF")
            ctext((bx + cw / 2, y + h / 2), c, F_CHIP, color)

    y += h + GAP

Y_ARCH_END = y - GAP

# ---------------------------------------------------------------- 时间线
Y_TL_TOP = Y_ARCH_END + 78
d.text((X0, Y_TL_TOP - 34), "0 → 1 时间线", font=F_LAYER, fill=INK)

PHASES = [
    ("P0 设计", "现在–1 月", "文档 · 路线图", "#94A3B8"),
    ("P1 内核", "1–6 月", "QEMU 启动 · 多核调度", "#DC2626"),
    ("P2 用户态", "6–12 月", "中文 shell 输命令", "#EA580C"),
    ("P3 图形", "12–20 月", "QEMU 出桌面", "#CA8A04"),
    ("P4 兼容", "20–30 月", "跑 busybox / Linux 程序", "#059669"),
    ("P5 日常", "30–48 月", "能当第二台电脑", "#0284C7"),
    ("P6 生态", "4–8 年", "AI 原生 · GUI 兼容", "#7C3AED"),
]

line_y = Y_TL_TOP + 92
tl_x0 = X0 + 60
tl_x1 = X1 - 60
d.line([tl_x0 - 40, line_y, tl_x1 + 40, line_y], fill="#CBD5E1", width=4)

n = len(PHASES)
step = (tl_x1 - tl_x0) / (n - 1)

for i, (pname, ptime, pnote, pcolor) in enumerate(PHASES):
    px = tl_x0 + i * step
    # 节点
    d.ellipse([px - 13, line_y - 13, px + 13, line_y + 13], fill=pcolor, outline="#FFFFFF", width=3)
    # 阶段名（上方）
    ctext((px, line_y - 52), pname, F_PHASE, INK)
    ctext((px, line_y - 28), ptime, F_TIME, MUTED)
    # 说明（下方）
    ctext((px, line_y + 38), pnote, F_NOTE, pcolor)

# ---------------------------------------------------------------- 底部说明
foot = "A 层（0–2 年）可交付 · B 层（2–5 年）要够一下 · C 层（5–10 年）是愿景——对外只承诺 A 层"
ctext((W / 2, H - 38), foot, F_LEGEND, MUTED)

img.save(OUT, "PNG")
print("saved:", OUT, img.size)
