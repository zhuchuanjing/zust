local Vec3 = {}
function Vec3.new(x, y, z)
    return {x = x, y = y, z = z}
end

local n = tonumber(arg[1])
local v = Vec3.new(1.0, 2.0, 3.0)
local total = 0.0
for _ = 0, n - 1 do
    local sum = v.x + v.y + v.z
    v.x = v.x + 0.001
    v.y = v.y + 0.002
    v.z = v.z + 0.003
    total = total + sum
end
print(math.floor(total))
