import sys

n = int(sys.argv[1])
result = 1
m = 1000000007
for i in range(1, n + 1):
    result = (result * i) % m
print(result)
