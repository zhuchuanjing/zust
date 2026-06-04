import sys

n = int(sys.argv[1])
acc = 0
def add(a, b):
    return a + b
for i in range(n):
    acc = add(acc, i)
print(acc)
