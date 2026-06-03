import sys

n = int(sys.argv[1])
x = 1.0
y = 2.0
for _ in range(n):
    x = x * 1.000001 + y * 0.999999
    y = y * 1.000001 - x * 0.999999
print(int(x + y))
