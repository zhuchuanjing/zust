local function make_tree(depth)
    if depth <= 0 then return 1 end
    return 1 + make_tree(depth - 1) + make_tree(depth - 1)
end
local n = tonumber(arg[1])
print(make_tree(n))
