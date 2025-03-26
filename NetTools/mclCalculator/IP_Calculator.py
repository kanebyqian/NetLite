import sys
import signal
import ipaddress

def IP_Calculator(ip_net):
    try:
        # 创建网络对象
        net = ipaddress.ip_network(ip_net, strict=False)
        
        # 输出分隔符
        separator = '*' * 50  # 你可以调整分隔符的数量
        
        # 合并输出内容
        output = [
            separator,
            f"IP版本号： {net.version}",
            f"是否是私有地址： {net.is_private}",
            f"网络号： {net.network_address}",
            f"前缀长度： {net.prefixlen}",
            f"子网掩码： {net.netmask}",
            f"反子网掩码： {net.hostmask}",
            f"IP地址总数: {net.num_addresses}",
        ]
        
        # 计算可用IP地址数
        total_ips = net.num_addresses
        usable_ips = total_ips - 2 if total_ips > 2 else 0
        output.append(f"可用IP地址总数： {usable_ips}")

        # 计算起始可用IP地址和最后可用IP地址
        if usable_ips > 0:
            first_usable_ip = net.network_address + 1  # 第一个可用IP（网络地址 + 1）
            last_usable_ip = net.broadcast_address - 1  # 最后一个可用IP（广播地址 - 1）
            output.append(f"起始可用IP地址： {first_usable_ip}")
            output.append(f"最后可用IP地址： {last_usable_ip}")
            output.append(f"可用IP地址范围： {first_usable_ip} ~ {last_usable_ip}")
        else:
            output.append("没有可用IP地址。")
        
        # 打印广播地址
        output.append(f"广播地址： {net.broadcast_address}")
        
        # 输出所有结果
        output.append(separator)
        print("\n".join(output))

    except ValueError:
        print('Error: Invalid input format. Use IP/mask or IP/subnet mask.')

def quit(signum, frame):
    print("\nBye! Hope to see you again!")
    sys.exit(0)

if __name__ == '__main__':
    signal.signal(signal.SIGINT, quit)
    print("欢迎使用本程序！")
    print("输入 'quit' 退出程序，按 Ctrl+C 强制退出。")
    
    # 循环执行，直到用户输入 'quit'
    while True:
        # 提示用户输入IP地址和子网掩码
        user_input = input("请输入IP/mask or IP/subnet：\n").strip()
        
        if user_input.lower() == 'quit':
            quit(None, None)  # 用户输入 quit，退出程序
        
        # 如果输入不是 'quit'，继续执行网络计算
        IP_Calculator(user_input)