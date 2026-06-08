import sys

n = int(sys.argv[1])
acc = 0
def sum16(a, b, c, d, e, f, g, h, i, j, k, l, m, n_arg, o, p):
    return a + b + c + d + e + f + g + h + i + j + k + l + m + n_arg + o + p
for idx in range(n):
    acc += sum16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16)
print(acc)
