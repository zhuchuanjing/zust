import sys

n = int(sys.argv[1])
total = 0
for start in range(1, n + 1):
    x = start
    while x != 1:
        if x % 2 == 0:
            x = x // 2
        else:
            x = 3 * x + 1
        total += 1
print(total)
