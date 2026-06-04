import sys
n = int(sys.argv[1])
arr = [i * 2 for i in range(n)]
total = 0
for target in range(n):
    low, high = 0, n - 1
    found = -1
    while low <= high:
        mid = (low + high) // 2
        if arr[mid] == target * 2:
            found = mid
            low = high + 1
        elif arr[mid] < target * 2:
            low = mid + 1
        else:
            high = mid - 1
    total += found
print(total)
