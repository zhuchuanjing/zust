import sys

n = int(sys.argv[1])
size = 100
a = [i * 1.5 for i in range(size)]
total = 0.0
for _ in range(n):
    for i in range(size):
        total += a[i]
print(int(total))
