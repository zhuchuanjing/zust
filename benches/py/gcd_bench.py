import sys

def gcd(a, b):
    while b != 0:
        a, b = b, a % b
    return a

n = int(sys.argv[1])
total = 0
for i in range(n):
    total += gcd(i, n - i)
print(total)
