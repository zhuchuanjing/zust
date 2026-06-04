import sys

n = int(sys.argv[1])
m = {}
for i in range(n):
    key = "key_" + str(i)
    m[key] = i
total = 0
for i in range(n):
    key = "key_" + str(i)
    total += m[key]
print(total)
