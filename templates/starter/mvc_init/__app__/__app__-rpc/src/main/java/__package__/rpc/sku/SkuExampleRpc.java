package __package__.rpc.sku;

import __package__.rpc.sku.dto.SkuExampleDTO;

import java.util.List;

/**
 * SkuExampleRpc
 *
 * @author weirenyan
 * @date 2023/12/6
 */
public interface SkuExampleRpc {

    /**
     * 根据skuid查询sku数据信息
     *
     * @param skuIds
     * @return
     */
    List<SkuExampleDTO> fetchSkuList(List<String> skuIds);
}
