import sys

def is_prime(n):
    if n < 2:
        return False
    i = 2
    while i * i <= n:
        if n % i == 0:
            return False
        i += 1
    return True

n = int(sys.argv[1])
count = 0
for x in range(2, n + 1):
    if is_prime(x):
        count += 1
print(count)
