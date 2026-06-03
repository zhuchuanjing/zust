import sys

n = int(sys.argv[1])
bodies = 100
total = 0
for step in range(n):
    for i in range(bodies):
        for j in range(bodies):
            if i != j:
                total += i * j
print(total)
