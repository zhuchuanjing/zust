import sys
n = int(sys.argv[1])
arr = list(range(1000))
total = 0
half = 500
for _ in range(n):
    for i in range(half):
        j = 999 - i
        arr[i], arr[j] = arr[j], arr[i]
    for i in range(1000):
        total += arr[i]
print(total)
