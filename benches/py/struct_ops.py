import sys

class Vec3:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

n = int(sys.argv[1])
v = Vec3(1.0, 2.0, 3.0)
total = 0.0
for _ in range(n):
    s = v.x + v.y + v.z
    v.x += 0.001
    v.y += 0.002
    v.z += 0.003
    total += s
print(int(total))
