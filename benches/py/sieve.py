import sys
n = int(sys.argv[1])
is_prime = [True] * (n + 1)
if n >= 0: is_prime[0] = False
if n >= 1: is_prime[1] = False
count = 0
for p in range(2, n + 1):
    if is_prime[p]:
        count += 1
        step = p
        j = p * p
        while j <= n:
            is_prime[j] = False
            j += step
print(count)
