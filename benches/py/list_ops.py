import sys
n = int(sys.argv[1])
l = []
for i in range(n): l.append(i)
total = 0
for i in range(n): total += l[i]
print(total)
