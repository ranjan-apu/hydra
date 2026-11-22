from PIL import Image, ImageDraw, ImageFilter

S = 1024
img = Image.new('RGBA', (S, S), (0, 0, 0, 0))

# vertical gradient base inside a rounded-rect mask (macOS-style squircle)
col = Image.new('RGBA', (1, S))
top = (21, 30, 54)
bot = (14, 59, 66)
for y in range(S):
    t = y / (S - 1)
    c = (int(top[0] + (bot[0] - top[0]) * t),
         int(top[1] + (bot[1] - top[1]) * t),
         int(top[2] + (bot[2] - top[2]) * t),
         255)
    col.putpixel((0, y), c)
grad = col.resize((S, S))

mask = Image.new('L', (S, S), 0)
md = ImageDraw.Draw(mask)
R = 232
md.rounded_rectangle([0, 0, S - 1, S - 1], radius=R, fill=255)
img.paste(grad, (0, 0), mask)

d = ImageDraw.Draw(img)
d.rounded_rectangle([4, 4, S - 5, S - 5], radius=R - 4, outline=(255, 255, 255, 26), width=3)

# three streams (the hydra heads) converge into one flow -> one arrow (the file)
streams = Image.new('RGBA', (S, S), (0, 0, 0, 0))
sd = ImageDraw.Draw(streams)
AC1 = (94, 160, 255, 255)
AC2 = (56, 217, 192, 255)
AC3 = (124, 156, 255, 255)
LIGHT = (232, 246, 255, 255)
DARKCORE = (16, 24, 40, 255)

def stream(p0, p1, w, color):
    sd.line([p0, p1], fill=color, width=w)
    r = w // 2
    sd.ellipse([p0[0] - r, p0[1] - r, p0[0] + r, p0[1] + r], fill=color)
    sd.ellipse([p1[0] - r, p1[1] - r, p1[0] + r, p1[1] + r], fill=color)

hub = (512, 470)
stream((218, 190), hub, 46, AC1)
stream((512, 140), hub, 46, AC2)
stream((806, 190), hub, 46, AC3)

for (hx, hy), color in (((218, 190), AC1), ((512, 140), AC2), ((806, 190), AC3)):
    sd.ellipse([hx - 58, hy - 58, hx + 58, hy + 58], fill=color)
    sd.ellipse([hx - 26, hy - 26, hx + 26, hy + 26], fill=DARKCORE)

stream(hub, (512, 660), 56, LIGHT)
sd.polygon([(512, 836), (400, 668), (624, 668)], fill=LIGHT)

glow = streams.filter(ImageFilter.GaussianBlur(22))
img.alpha_composite(glow)
img.alpha_composite(glow)
img.alpha_composite(streams)
img.save('assets/icon.png')
print('saved assets/icon.png', img.size)
