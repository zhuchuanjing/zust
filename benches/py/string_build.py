import sys

n = int(sys.argv[1])
s = ""
sep = ","
chunk = "hello"
for i in range(n):
    if i > 0:
        s += sep
    s += chunk
print(len(s))
