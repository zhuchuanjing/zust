import sys

n = int(sys.argv[1])
total = 0
for y in range(n):
    for x in range(n):
        cr = 1.5 * (x - n / 2) / (0.5 * n)
        ci = (y - n / 2) / (0.5 * n)
        zr = zi = 0.0
        k = 0
        while k < 50 and zr * zr + zi * zi < 4:
            zr, zi = zr * zr - zi * zi + cr, 2 * zr * zi + ci
            k += 1
        total += k
print(total)
